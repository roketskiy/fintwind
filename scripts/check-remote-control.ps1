param(
    [string]$ArtifactDirectory = '',
    [switch]$BuildRelease
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (-not $ArtifactDirectory) {
    $stamp = (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N')
    $ArtifactDirectory = Join-Path 'C:/Users/Public/Temp/opencode' "remote-check-$stamp"
}
$null = New-Item -ItemType Directory -Path $ArtifactDirectory -Force
$checks = [System.Collections.Generic.List[object]]::new()

function Get-SourceHashes {
    $files = @(& git ls-files --cached --others --exclude-standard -- 'Cargo.toml' 'Cargo.lock' 'crates/fintwind-remote-host' 'crates/fintwind-relay' 'crates/fintwind-remote-protocol' 'crates/fintwind-remote-crypto' 'crates/fintwind-remote-crypto-wasm' 'crates/fintwind-daemon' 'crates/fintwind-protocol' 'crates/fintwind-client' 'crates/fintwind-core' 'locales' 'scripts/fixtures/remote-control')
    if ($LASTEXITCODE -ne 0) { throw 'Source enumeration failed.' }
    $hashes = [ordered]@{}
    foreach ($file in ($files | Sort-Object -Unique)) {
        if ($file -match '\.(rs|toml|lock|yml|json|py|cmd|ps1|mjs)$') {
            $absolute = Join-Path $repo $file
            if (Test-Path -LiteralPath $absolute -PathType Leaf) {
                $hashes[$file] = (Get-FileHash -LiteralPath $absolute -Algorithm SHA256).Hash
            }
        }
    }
    return $hashes
}

function Get-BinaryHashes {
    $hashes = [ordered]@{}
    foreach ($profile in @('debug', 'release')) {
        if (($profile -eq 'release') -and (-not $BuildRelease)) { continue }
        $directory = Join-Path (Join-Path $repo 'target') $profile
        foreach ($name in @('fintwind-daemon', 'fintwind-remote-host', 'fintwind-relay')) {
            $binary = Join-Path $directory ($name + '.exe')
            if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'A checked binary is missing.' }
            $hashes[($profile + '/' + $name)] = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
        }
    }
    return $hashes
}

function Invoke-Check {
    param([string]$Name, [string[]]$Arguments)
    $started = [DateTimeOffset]::UtcNow
    Write-Host "[CHECK] $Name"
    & cargo @Arguments
    $code = $LASTEXITCODE
    $checks.Add([ordered]@{
        name = $Name
        command = 'cargo ' + ($Arguments -join ' ')
        startedAt = $started.ToString('o')
        elapsedSeconds = [math]::Round(([DateTimeOffset]::UtcNow - $started).TotalSeconds, 2)
        exitCode = $code
        passed = $code -eq 0
    })
    if ($code -ne 0) {
        throw "Check failed: $Name (exit $code)."
    }
}

$failed = $false
$head = $null
$status = @()
$sourceHashes = [ordered]@{}
$binaryHashes = [ordered]@{}
$phase = 'initialization'
$startedAt = [DateTimeOffset]::UtcNow.ToString('o')
$priorStressOutput = $env:FINTWIND_STRESS_OUT
Push-Location $repo
try {
    $env:FINTWIND_STRESS_OUT = Join-Path $ArtifactDirectory 'relay-stress'
    $head = & git rev-parse HEAD
    $status = @(& git status --porcelain)
    $sourceHashes = Get-SourceHashes
    $phase = 'native-and-wasm-checks'
    Invoke-Check 'remote-native-check' @('check', '--locked', '-p', 'fintwind-relay', '-p', 'fintwind-remote-host')
    Invoke-Check 'remote-wasm-check' @('check', '--locked', '-p', 'fintwind-remote-crypto-wasm', '--target', 'wasm32-unknown-unknown')
    Invoke-Check 'remote-blackbox-binaries' @('build', '--locked', '-p', 'fintwind-relay', '--bin', 'fintwind-relay', '-p', 'fintwind-remote-host', '--bin', 'fintwind-remote-host', '-p', 'fintwind-daemon', '--bin', 'fintwind-daemon')
    $phase = 'external-process-blackboxes'
    Invoke-Check 'remote-host-blackbox' @('test', '--locked', '-p', 'fintwind-remote-host', '--test', 'device_loop', '--test', 'pairing_loop')
    Invoke-Check 'real-daemon-isolation' @('test', '--locked', '-p', 'fintwind-remote-host', '--test', 'isolated_daemon')
    Invoke-Check 'relay-online-resource-blackbox' @('test', '--locked', '-p', 'fintwind-relay', '--test', 'relay_blackbox', '--', '--test-threads=1')
    Invoke-Check 'relay-global-admission-stress' @('test', '--locked', '-p', 'fintwind-relay', '--test', 'quota_stress', '--', '--test-threads=1')
    if ($BuildRelease) {
        $phase = 'release-build'
        Invoke-Check 'remote-release-build' @('build', '--locked', '--release', '-p', 'fintwind-daemon', '--bin', 'fintwind-daemon', '-p', 'fintwind-remote-host', '--bin', 'fintwind-remote-host', '-p', 'fintwind-relay', '--bin', 'fintwind-relay')
    }
    $afterHashes = Get-SourceHashes
    $beforeJson = $sourceHashes | ConvertTo-Json -Depth 10 -Compress
    $afterJson = $afterHashes | ConvertTo-Json -Depth 10 -Compress
    if ($beforeJson -ne $afterJson) {
        throw 'Sources changed while checks ran. Rerun against the final code state.'
    }
    $binaryHashes = Get-BinaryHashes
    $phase = $null
}
catch {
    $failed = $true
    Write-Error -ErrorAction Continue '[X] Remote-control validation failed. See the failing command above.'
}
finally {
    $report = [ordered]@{
        capturedAt = [DateTimeOffset]::UtcNow.ToString('o')
        startedAt = $startedAt
        failedPhase = $phase
        repository = $repo
        head = $head
        workingTree = $status
        sourceSha256 = $sourceHashes
        binarySha256 = $binaryHashes
        runnerSha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
        checks = @($checks.ToArray())
        passed = -not $failed
        publicChainVerified = $false
        note = 'Native/WASM builds, isolated device_loop/pairing_loop, real-daemon directory isolation and relay resource/quota blackboxes only. daemon_smoke is intentionally excluded because it mutates production routing metadata. Public browser/relay/host/daemon E2E must run separately against final artifacts. Ignored tests never count as acceptance.'
    }
    $reportPath = Join-Path $ArtifactDirectory 'summary.json'
    $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reportPath -Encoding utf8
    Write-Host "[ARTIFACT] $reportPath"
    $env:FINTWIND_STRESS_OUT = $priorStressOutput
    Pop-Location
}
if ($failed) { exit 1 }
Write-Host '[OK] Local remote-control checks passed.'
