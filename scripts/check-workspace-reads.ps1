param([string]$ArtifactDirectory = '')

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$pwa = Join-Path (Split-Path -Parent $repo) 'fintwind-pwa'
if (-not $ArtifactDirectory) {
    $ArtifactDirectory = Join-Path 'C:/Users/Public/Temp/opencode' ('workspace-read-check-' + [guid]::NewGuid().ToString())
}
$null = New-Item -ItemType Directory -Path $ArtifactDirectory -Force
$report = [ordered]@{
    startedAt = [DateTimeOffset]::UtcNow.ToString('o')
    passed = $false
    publicChainVerified = $false
    buildExitCode = $null
    blackboxExitCode = $null
    sourceSha256 = [ordered]@{}
    failedPhase = $null
}

function Get-InputHashes {
    $inputs = @(& git ls-files --cached --others --exclude-standard -- 'Cargo.toml' 'Cargo.lock' 'crates/fintwind-remote-host' 'crates/fintwind-remote-protocol' 'crates/fintwind-remote-crypto' 'crates/fintwind-protocol' 'crates/fintwind-client' 'locales')
    if ($LASTEXITCODE -ne 0) { throw 'Probe input enumeration failed.' }
    $hashes = [ordered]@{}
    foreach ($file in ($inputs | Sort-Object -Unique)) {
        if ($file -match '\.(rs|toml|lock|yml)$') {
            $absolute = Join-Path $repo $file
            if (Test-Path -LiteralPath $absolute -PathType Leaf) {
                $hashes[$file] = (Get-FileHash -LiteralPath $absolute -Algorithm SHA256).Hash
            }
        }
    }
    return $hashes
}

Push-Location $repo
try {
    $before = Get-InputHashes
    $report.sourceSha256 = $before
    $report.failedPhase = 'build'
    & cargo build --locked -p fintwind-remote-host --example workspace_read_probe
    $report.buildExitCode = $LASTEXITCODE
    if ($LASTEXITCODE -ne 0) { throw 'Workspace probe build failed.' }
    $afterBuild = Get-InputHashes
    if (($before | ConvertTo-Json -Depth 10 -Compress) -ne ($afterBuild | ConvertTo-Json -Depth 10 -Compress)) {
        throw 'Compiler inputs changed during the build. Run again against the final code state.'
    }
    $report.failedPhase = 'blackbox'
    $script = Join-Path $pwa 'scripts/workspace-reads-blackbox.mjs'
    $report.runnerSha256 = (Get-FileHash -LiteralPath $script -Algorithm SHA256).Hash
    $report.wrapperSha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
    & node $script
    $report.blackboxExitCode = $LASTEXITCODE
    if ($LASTEXITCODE -ne 0) { throw 'Workspace read behavior failed.' }
    $afterChecks = Get-InputHashes
    if (($before | ConvertTo-Json -Depth 10 -Compress) -ne ($afterChecks | ConvertTo-Json -Depth 10 -Compress)) {
        throw 'Compiler inputs changed during behavior checks. Run again against final code state.'
    }
    $report.passed = $true
    $report.failedPhase = $null
}
catch {
    Write-Host '[X] Workspace validation failed. The failing phase is recorded; raw errors were not saved.'
}
finally {
    Pop-Location
    $report.finishedAt = [DateTimeOffset]::UtcNow.ToString('o')
    $reportPath = Join-Path $ArtifactDirectory 'summary.json'
    $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reportPath -Encoding utf8
    Write-Host "[ARTIFACT] $reportPath"
}
if (-not $report.passed) { exit 1 }
Write-Host '[OK] Whole-module workspace read validation passed.'
