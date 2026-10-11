param([string]$LaunchReport)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$base = 'C:/Users/Public/Temp/opencode'
$repo = Split-Path -Parent $PSScriptRoot
$runner = Join-Path $PSScriptRoot 'run-normal-remote-suite.mjs'
$launch = Join-Path $base ('normal-suite-launch-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $launch
if (-not $LaunchReport) { $LaunchReport = Join-Path $launch 'launch.json' }
$fullReport = [System.IO.Path]::GetFullPath($LaunchReport)
if (-not $fullReport.StartsWith([System.IO.Path]::GetFullPath($base) + [System.IO.Path]::DirectorySeparatorChar,
    [System.StringComparison]::OrdinalIgnoreCase)) { throw 'An isolated report location is required.' }
if (Test-Path -LiteralPath $fullReport) { throw 'An existing report must not be overwritten.' }
$result = [ordered]@{
    artifact = 'normal-remote-suite-launch'
    startedAt = [DateTime]::UtcNow.ToString('o')
    passed = $false
    status = 'launcher-failed'
    summary = $null
    exitCode = $null
    maliciousTestsEnabled = $false
    fullPlanVerified = $false
    publicChainVerified = $false
    launcherSha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
    suiteSha256 = (Get-FileHash -LiteralPath $runner -Algorithm SHA256).Hash.ToLowerInvariant()
}
$process = $null
$code = 1
try {
    $node = (Get-Command node -ErrorAction Stop).Source
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $node
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $repo
    $start.ArgumentList.Add($runner)
    $start.Environment.Clear()
    foreach ($name in @('SystemRoot', 'windir', 'COMSPEC', 'PATH', 'PATHEXT', 'NUMBER_OF_PROCESSORS')) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if ($value) { $start.Environment[$name] = $value }
    }
    $homeDirectory = Join-Path $launch 'home'
    $null = New-Item -ItemType Directory -Path $homeDirectory
    foreach ($name in @('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP')) {
        $start.Environment[$name] = $homeDirectory
    }
    $process = [System.Diagnostics.Process]::Start($start)
    if (-not $process) { throw 'The hidden suite did not start.' }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(2100000)) {
        $result.status = 'launcher-deadline-unverified'
        throw 'The bounded launcher wait expired; do not expand process cleanup.'
    }
    $text = $stdout.GetAwaiter().GetResult()
    $null = $stderr.GetAwaiter().GetResult()
    $result.exitCode = $process.ExitCode
    foreach ($line in ($text -split '[\r\n]+')) {
        if ($line -match '^\[SUITE\] (C:[/\\]Users[/\\]Public[/\\]Temp[/\\]opencode[/\\]normal-remote-suite-[a-zA-Z0-9]+[/\\]summary.json)$') {
            $result.summary = $Matches[1]
        }
    }
    if (-not $result.summary) { throw 'No redacted suite summary was produced.' }
    $summary = Get-Content -LiteralPath $result.summary -Raw | ConvertFrom-Json
    if (-not $summary) { throw 'The suite summary is invalid.' }
    $result.passed = ($process.ExitCode -eq 0) -and ($summary.passed -eq $true) -and
        ($summary.batchPlanVerified -eq $true) -and ($summary.inputsUnchanged -eq $true)
    if ($result.passed) {
        $result.status = 'normal-batch-passed-not-full-plan'
        $code = 0
    } elseif ($summary.PSObject.Properties['blocked']) {
        $result.status = 'batch-blocked'
        $code = 2
    } else {
        $result.status = 'normal-batch-failed'
    }
    if ((Get-FileHash -LiteralPath $runner -Algorithm SHA256).Hash.ToLowerInvariant() -ne $result.suiteSha256) {
        $result.passed = $false
        $result.status = 'suite-changed'
        $code = 1
    }
} catch {
    $result.passed = $false
    $code = 1
} finally {
    if ($process) { $process.Dispose() }
    $result.finishedAt = [DateTime]::UtcNow.ToString('o')
    $bytes = [System.Text.UTF8Encoding]::new($false).GetBytes(($result | ConvertTo-Json -Depth 10))
    $stream = [System.IO.File]::Open($fullReport, [System.IO.FileMode]::CreateNew,
        [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
    try { $stream.Write($bytes, 0, $bytes.Length) } finally { $stream.Dispose() }
}
if ($result.summary) { Write-Output ('[SUITE] ' + $result.summary) }
exit $code
