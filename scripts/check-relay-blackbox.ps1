param(
    [string]$OutputDirectory = 'C:/Users/Public/Temp/opencode',
    [string]$Case = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$round = Join-Path $OutputDirectory ('relay-blackbox-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $round
$report = [ordered]@{
    startedAt = [DateTimeOffset]::UtcNow.ToString('o')
    passed = $false
    fullPlanVerified = $false
    publicChainVerified = $false
    failedPhase = 'build'
    sourceSha256 = [ordered]@{}
    binarySha256 = $null
    runnerSha256 = $null
    allOwnedProcessesStopped = $false
    exitCode = $null
    cases = @()
    caseFilter = $Case
    command = 'cargo test --locked -p fintwind-relay --test relay_blackbox --no-run --message-format=json, then frozen relay_blackbox --test-threads=1'
}
function Get-Inputs {
    $files = @(& git ls-files --cached --others --exclude-standard -- 'Cargo.toml' 'Cargo.lock' 'crates/fintwind-relay' 'crates/fintwind-remote-protocol' 'crates/fintwind-remote-crypto' 'crates/fintwind-protocol' 'locales' 'scripts/check-relay-blackbox.ps1')
    if ($LASTEXITCODE -ne 0) { throw 'Relay build input enumeration failed.' }
    $hashes = [ordered]@{}
    foreach ($file in ($files | Sort-Object -Unique)) {
        if ($file -match '\.(rs|toml|lock|yml|ps1)$') {
            $hashes[$file] = (Get-FileHash -LiteralPath (Join-Path $repo $file) -Algorithm SHA256).Hash
        }
    }
    return $hashes
}
$runner = $null
Push-Location $repo
try {
    $before = Get-Inputs
    $report.sourceSha256 = $before
    $messages = @(& cargo test --locked -p fintwind-relay --test relay_blackbox --no-run --message-format=json)
    if ($LASTEXITCODE -ne 0) { throw 'Relay network build failed.' }
    $artifacts = @($messages | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object { $_ -and $_.reason -eq 'compiler-artifact' -and $_.executable })
    $binaries = @($artifacts | Where-Object { $_.target.name -eq 'fintwind-relay' })
    $runners = @($artifacts | Where-Object { $_.target.name -eq 'relay_blackbox' })
    if (($binaries.Count -ne 1) -or ($runners.Count -ne 1)) { throw 'The relay build did not produce unique executables.' }
    $report.failedPhase = 'build-source-drift'
    $after = Get-Inputs
    if (($before | ConvertTo-Json -Depth 10 -Compress) -ne ($after | ConvertTo-Json -Depth 10 -Compress)) { throw 'Relay inputs changed during the build.' }
    $frozenBinary = Join-Path $round 'fintwind-relay.exe'
    $frozenRunner = Join-Path $round 'relay_blackbox.exe'
    foreach ($pair in @(@($binaries[0].executable, $frozenBinary), @($runners[0].executable, $frozenRunner))) {
        $hash = (Get-FileHash -LiteralPath $pair[0] -Algorithm SHA256).Hash
        Copy-Item -LiteralPath $pair[0] -Destination $pair[1]
        if ((Get-FileHash -LiteralPath $pair[1] -Algorithm SHA256).Hash -ne $hash) { throw 'An executable changed while freezing it.' }
    }
    $report.binarySha256 = (Get-FileHash -LiteralPath $frozenBinary -Algorithm SHA256).Hash
    $report.runnerSha256 = (Get-FileHash -LiteralPath $frozenRunner -Algorithm SHA256).Hash
    $report.failedPhase = 'native-network-blackbox'
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $frozenRunner
    $start.WorkingDirectory = $round
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.Environment.Clear()
    foreach ($name in @('PATH', 'PATHEXT', 'SystemRoot', 'WINDIR', 'ComSpec', 'SYSTEMDRIVE')) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if ($value) { $start.Environment[$name] = $value }
    }
    $isolatedHome = Join-Path $round 'home'
    $null = New-Item -ItemType Directory -Path $isolatedHome
    foreach ($name in @('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'XDG_CONFIG_HOME', 'TEMP', 'TMP')) { $start.Environment[$name] = $isolatedHome }
    $start.Environment['FINTWIND_RELAY_BLACKBOX_OUT'] = $round
    $start.Environment['FINTWIND_RELAY_BLACKBOX_BINARY'] = $frozenBinary
    $start.ArgumentList.Add('--test-threads=1')
    if ($Case) { $start.ArgumentList.Add($Case) }
    $runner = [System.Diagnostics.Process]::Start($start)
    $output = $runner.StandardOutput.ReadToEndAsync()
    $errors = $runner.StandardError.ReadToEndAsync()
    if (-not $runner.WaitForExit(900000)) {
        $report.failedPhase = 'blackbox-deadline'
        $runner.Kill($true)
        $runner.WaitForExit()
        throw 'The relay blackbox exceeded its total deadline.'
    }
    $report.exitCode = $runner.ExitCode
    $null = $errors.GetAwaiter().GetResult()
    $text = $output.GetAwaiter().GetResult()
    foreach ($line in ($text -split '\r?\n')) {
        if ($line -match '^test ([a-zA-Z0-9_]+) \.\.\. (ok|FAILED)$') {
            $report.cases += [ordered]@{ name = $Matches[1]; passed = $Matches[2] -eq 'ok' }
            Write-Host ('[' + $Matches[2] + '] ' + $Matches[1])
        }
    }
    $instances = @(Get-ChildItem -LiteralPath $round -Directory -Filter 'fintwind-relay-e2e-*')
    $cleanupReports = @($instances | ForEach-Object {
        $lifecycle = Join-Path $_.FullName 'lifecycle.json'
        if (Test-Path -LiteralPath $lifecycle) { Get-Content -LiteralPath $lifecycle -Raw | ConvertFrom-Json }
    })
    $report.allOwnedProcessesStopped = ($cleanupReports.Count -eq $instances.Count) -and ($cleanupReports.Count -gt 0) -and (@($cleanupReports | Where-Object { -not $_.allOwnedProcessesStopped }).Count -eq 0)
    if (($report.exitCode -ne 0) -or ($report.cases.Count -eq 0)) { throw 'Relay network behavior failed.' }
    $report.failedPhase = 'process-cleanup'
    if (-not $report.allOwnedProcessesStopped) { throw 'Relay process cleanup evidence is incomplete.' }
    $report.failedPhase = 'behavior-source-drift'
    $after = Get-Inputs
    if (($before | ConvertTo-Json -Depth 10 -Compress) -ne ($after | ConvertTo-Json -Depth 10 -Compress)) { throw 'Relay inputs changed during the behavior run.' }
    if (((Get-FileHash -LiteralPath $frozenBinary -Algorithm SHA256).Hash -ne $report.binarySha256) -or ((Get-FileHash -LiteralPath $frozenRunner -Algorithm SHA256).Hash -ne $report.runnerSha256)) { throw 'A frozen executable changed.' }
    $report.passed = $true
    $report.failedPhase = $null
}
catch { Write-Host '[X] Relay network validation failed; see the phase in the report.' }
finally {
    if ($runner -and (-not $runner.HasExited)) { $runner.Kill($true); $runner.WaitForExit() }
    Pop-Location
    $report.finishedAt = [DateTimeOffset]::UtcNow.ToString('o')
    $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $round 'summary.json') -Encoding utf8
    Write-Host ('[ARTIFACT] ' + (Join-Path $round 'summary.json'))
}
if (-not $report.passed) { exit 1 }
Write-Host '[OK] Real relay network behavior passed.'
