param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Test', 'StableRelease')]
    [string]$Mode,
    [Parameter(Mandatory = $true)]
    [string]$LogDirectory,
    [ValidateRange(1, 5400)]
    [int]$TimeoutSeconds = 3000
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$LogDirectory = [System.IO.Path]::GetFullPath($LogDirectory)
New-Item -ItemType Directory -Path $LogDirectory -Force | Out-Null
$stdoutPath = Join-Path $LogDirectory 'cargo.stdout.log'
$stderrPath = Join-Path $LogDirectory 'cargo.stderr.log'
$summaryPath = Join-Path $LogDirectory 'fingerprint-summary.log'
$metadataPath = Join-Path $LogDirectory 'diagnostics.json'
$profile = 'debug'
$cargoArguments = @('test', '--locked')
if ($Mode -eq 'StableRelease') {
    $profile = 'release'
    $cargoArguments = @('check', '--locked', '--release', '--package', 'fintwind', '--bin', 'fintwind', '--package', 'fintwind-daemon', '--bin', 'fintwind-daemon')
}

function Get-Stamp {
    param([string]$Path, [bool]$Hash = $false)
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $item = Get-Item -LiteralPath $Path
    $value = [ordered]@{
        path = $item.FullName
        modifiedUtc = $item.LastWriteTimeUtc.ToString('O')
        modifiedTicks = $item.LastWriteTimeUtc.Ticks
        directory = $item.PSIsContainer
    }
    if ($Hash -and (-not $item.PSIsContainer)) {
        $value['sha256'] = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    }
    [pscustomobject]$value
}

$target = Join-Path (Get-Location).Path 'target'
if ($env:CARGO_TARGET_DIR) {
    $target = [System.IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)
}
$sourceStamps = @()
foreach ($path in @(
    'Cargo.toml', 'Cargo.lock', '.cargo/config.toml', 'build.rs', 'src/lib.rs',
    'vendor/gpui-pre/gpui-pre-0.3.8/Cargo.toml',
    'vendor/gpui-pre/gpui-pre-0.3.8/build.rs',
    'vendor/gpui-pre/gpui-pre-0.3.8/src/lib.rs',
    'vendor/gpui-pre/gpui-pre-windows-0.3.8/Cargo.toml',
    'vendor/gpui-pre/gpui-pre-windows-0.3.8/build.rs',
    'vendor/gpui-pre/gpui-pre-windows-0.3.8/src/lib.rs'
)) {
    $stamp = Get-Stamp -Path $path -Hash $true
    if ($stamp) { $sourceStamps += $stamp }
}
$fingerprintStamps = @()
$fingerprints = Join-Path (Join-Path $target $profile) '.fingerprint'
if (Test-Path -LiteralPath $fingerprints) {
    foreach ($directory in (Get-ChildItem -LiteralPath $fingerprints -Directory -Filter 'gpui-pre-*')) {
        foreach ($file in (Get-ChildItem -LiteralPath $directory.FullName -File)) {
            $fingerprintStamps += Get-Stamp -Path $file.FullName
        }
    }
}
$recordedEnvironment = [ordered]@{}
foreach ($name in @(
    'CARGO_INCREMENTAL', 'CARGO_PROFILE_DEV_DEBUG', 'CARGO_PROFILE_TEST_DEBUG',
    'CARGO_PROFILE_DEV_OPT_LEVEL', 'CARGO_PROFILE_TEST_OPT_LEVEL',
    'CARGO_TARGET_DIR', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTC_WRAPPER',
    'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_TARGET', 'CARGO_BUILD_JOBS'
)) {
    $recordedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}
$previousLog = $env:CARGO_LOG
$previousColor = $env:CARGO_TERM_COLOR
# Cargo renamed the module between versions. These narrow filters cover both
# names without enabling network, registry authentication, or general tracing.
$env:CARGO_LOG = 'cargo::core::compiler::fingerprint=trace,cargo::compiler::fingerprint=trace'
$env:CARGO_TERM_COLOR = 'never'
$started = [DateTime]::UtcNow
$exitCode = 1
$failure = $null
try {
    $cargoPath = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
    $cargoVersion = @(& cargo --version --verbose 2>&1 | ForEach-Object { $_.ToString() })
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read Cargo version' }
    $rustcVersion = @(& rustc --version --verbose 2>&1 | ForEach-Object { $_.ToString() })
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read rustc version' }
    Write-Output ('[INFO] Cargo fingerprint diagnostics: ' + $LogDirectory)
    & python (Join-Path $PSScriptRoot 'cargo-cache-live.py') --mode $Mode --log-directory $LogDirectory --timeout-seconds $TimeoutSeconds
    $exitCode = $LASTEXITCODE
} catch {
    $failure = $_.Exception.Message
    Write-Warning $failure
} finally {
    $env:CARGO_LOG = $previousLog
    $env:CARGO_TERM_COLOR = $previousColor
    $completed = [DateTime]::UtcNow
    $metadata = [ordered]@{
        mode = $Mode
        command = @('cargo') + $cargoArguments
        cargoPath = $cargoPath
        cargoVersion = $cargoVersion
        rustcVersion = $rustcVersion
        commit = $env:GITHUB_SHA
        workingDirectory = (Get-Location).Path
        targetDirectory = $target
        profile = $profile
        startedUtc = $started.ToString('O')
        completedUtc = $completed.ToString('O')
        seconds = [math]::Round(($completed - $started).TotalSeconds, 3)
        exitCode = $exitCode
        error = $failure
        environment = $recordedEnvironment
        sourceStampsBeforeCargo = $sourceStamps
        fingerprintStampsBeforeCargo = $fingerprintStamps
        phases = $null
    }
    $phasesPath = Join-Path $LogDirectory 'phases.json'
    if (Test-Path -LiteralPath $phasesPath) {
        $metadata.phases = Get-Content -LiteralPath $phasesPath -Raw | ConvertFrom-Json
    }
    $metadata | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $metadataPath -Encoding utf8
    if (Test-Path -LiteralPath $stderrPath) {
        Get-Content -LiteralPath $stderrPath |
            Select-String -Pattern 'dirty:|stale:|stale |FsStatusOutdated|fingerprint error|dependency.*changed|Finished |error:' |
            ForEach-Object { $_.Line } |
            Set-Content -LiteralPath $summaryPath -Encoding utf8
        Get-Content -LiteralPath $summaryPath -ErrorAction SilentlyContinue | Select-Object -First 40
    }
    if (Test-Path -LiteralPath $stdoutPath) { Get-Content -LiteralPath $stdoutPath | Select-Object -Last 25 }
    Write-Output ('[INFO] Cargo exit code: ' + $exitCode)
    if ($env:GITHUB_STEP_SUMMARY) {
        $lines = @(('### Cargo ' + $Mode), '', '| Result | Value |', '| --- | --- |',
            ('| Exit code | ' + $exitCode + ' |'), ('| Total seconds | ' + $metadata.seconds + ' |'))
        if ($metadata.phases) {
            $lines += ('| Compile seconds | ' + $metadata.phases.compileSeconds + ' |')
            $lines += ('| Tests and finalization seconds | ' + $metadata.phases.afterCompileSeconds + ' |')
        }
        $lines | Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Encoding utf8
    }
}
exit $exitCode
