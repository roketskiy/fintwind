param([string]$OutputDirectory = 'C:/Users/Public/Temp/opencode')

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$round = Join-Path $OutputDirectory ('remote-trust-build-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $round
$report = [ordered]@{
    startedAt = [DateTimeOffset]::UtcNow.ToString('o')
    passed = $false
    fullPlanVerified = $false
    publicChainVerified = $false
    failedPhase = 'build'
    sourceSha256 = [ordered]@{}
    binarySha256 = $null
    behaviorReport = $null
}
function Get-Inputs {
    $files = @(& git ls-files --cached --others --exclude-standard -- 'Cargo.toml' 'Cargo.lock' 'crates/fintwind-remote-host' 'crates/fintwind-remote-protocol' 'crates/fintwind-remote-crypto' 'crates/fintwind-protocol' 'crates/fintwind-client' 'locales' 'scripts/check-remote-trust.ps1' 'scripts/check-remote-trust-build.ps1')
    if ($LASTEXITCODE -ne 0) { throw 'Trust build input enumeration failed.' }
    $hashes = [ordered]@{}
    foreach ($file in ($files | Sort-Object -Unique)) {
        if ($file -match '\.(rs|toml|lock|yml|ps1)$') {
            $hashes[$file] = (Get-FileHash -LiteralPath (Join-Path $repo $file) -Algorithm SHA256).Hash
        }
    }
    return $hashes
}
Push-Location $repo
try {
    $before = Get-Inputs
    $report.sourceSha256 = $before
    & cargo build --locked -p fintwind-remote-host --bin fintwind-remote-host
    if ($LASTEXITCODE -ne 0) { throw 'Trust binary build failed.' }
    $report.failedPhase = 'build-input-drift'
    $after = Get-Inputs
    if (($before | ConvertTo-Json -Depth 10 -Compress) -ne ($after | ConvertTo-Json -Depth 10 -Compress)) { throw 'Trust compiler inputs changed during the build.' }
    $binary = Join-Path (Join-Path (Join-Path $repo 'target') 'debug') 'fintwind-remote-host.exe'
    $frozen = Join-Path $round 'fintwind-remote-host.exe'
    $builtHash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
    Copy-Item -LiteralPath $binary -Destination $frozen
    $report.binarySha256 = (Get-FileHash -LiteralPath $frozen -Algorithm SHA256).Hash
    if ($builtHash -ne $report.binarySha256) { throw 'The trust binary changed while freezing it.' }
    $report.failedPhase = 'blackbox'
    & pwsh -NoProfile -File (Join-Path $PSScriptRoot 'check-remote-trust.ps1') -HostBinary $frozen -ArtifactDirectory (Join-Path $round 'behavior')
    $exitCode = $LASTEXITCODE
    $behaviorDirectories = @(Get-ChildItem -LiteralPath $round -Directory -Filter 'behavior-*')
    if ($behaviorDirectories.Count -ne 1) { throw 'No unique trust behavior report was produced.' }
    $report.behaviorReport = Join-Path $behaviorDirectories[0].FullName 'summary.json'
    $behavior = Get-Content -LiteralPath $report.behaviorReport -Raw | ConvertFrom-Json
    if (-not $behavior) { throw 'The trust behavior report is empty.' }
    if (($exitCode -ne 0) -or ($behavior.passed -ne $true) -or ($behavior.allOwnedProcessesStopped -ne $true)) { throw 'The trust behavior did not pass and clean up.' }
    $report.failedPhase = 'behavior-input-drift'
    $after = Get-Inputs
    if (($before | ConvertTo-Json -Depth 10 -Compress) -ne ($after | ConvertTo-Json -Depth 10 -Compress)) { throw 'Trust inputs changed during the behavior run.' }
    if ((Get-FileHash -LiteralPath $frozen -Algorithm SHA256).Hash -ne $report.binarySha256) { throw 'The frozen trust binary changed.' }
    $report.passed = $true
    $report.failedPhase = $null
}
catch { Write-Host '[X] Trust build or behavior validation failed; see the phase in the report.' }
finally {
    Pop-Location
    $report.finishedAt = [DateTimeOffset]::UtcNow.ToString('o')
    $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $round 'summary.json') -Encoding utf8
    Write-Host ('[ARTIFACT] ' + (Join-Path $round 'summary.json'))
}
if (-not $report.passed) { exit 1 }
Write-Host '[OK] Frozen trust binary and behavior validation passed.'
