# Builds the real wasm bridge artifact with wasm-pack, then runs the
# black-box E2E: the device side is the actual wasm module the PWA
# loads, driven in Node; the host side is the Rust test. Every round
# builds into its own directory — a prior round's artifact and any
# production build output are evidence and must not be overwritten.
#
# STATUS: HELD. The entry below carries a no-bypass hold: until main
# lifts it (a validated owned-process launcher / job containment is
# pending), this script must not start children, alter the environment,
# or claim any verification. Everything past the hold is the repaired
# runner body — reachable only after the hold block is removed.
$ErrorActionPreference = 'Stop'

$repo = git -C $PSScriptRoot rev-parse --show-toplevel
if ($LASTEXITCODE -ne 0) { throw 'not inside a git work tree' }

$crate = Join-Path $repo 'crates\fintwind-remote-crypto-wasm'
$base = Join-Path 'C:\Users\Public\Temp\opencode' 'fintwind-e2e-wasm'
$round = Join-Path $base ("round-" + [guid]::NewGuid().ToString("N").Substring(0, 12))
New-Item -ItemType Directory -Force -Path $round | Out-Null
$pkg = Join-Path $round 'pkg'
New-Item -ItemType Directory -Force -Path $pkg | Out-Null
$summary = Join-Path $round 'run-summary.json'

# ----------------------------------------------------------------------
# NO-BYPASS HOLD — main lifts this by deleting this block. No argument
# and no environment value can skip it.
[ordered]@{
    stage = 'wasm-bridge-e2e-run'
    blocked = $true
    blockedReason = ('runner repairs (source-hash before run, full git input enumeration, ' +
        'per-stage unique logs, in-memory redaction, environment whitelist) are written ' +
        'but unproven, and process containment still relies on a handle kill rather than ' +
        'an owned job object. main lifts this hold after the ownedprocesslauncher review; ' +
        'there is no bypass.')
    processContainment = 'pending: ownedprocesslauncher (job object) — transitional handle kill only'
    fullPlanVerified = $false
    publicChainVerified = $false
    roundDirectory = $round
    wasmArtifact = $pkg
} | ConvertTo-Json -Depth 10 | Set-Content -Path $summary
Write-Host "[HOLD] wasm bridge e2e runner blocked pending main's review: $round"
exit 2

# ----------------------------------------------------------------------
# Runner body (unreachable while the hold above stands).
$homeDir = Join-Path $round 'home'
$tmpDir = Join-Path $round 'tmp'
$targetDir = Join-Path $round 'target'

# Full dependency enumeration, same method as main's wrapper: everything
# tracked or new-but-ignored under the crates the bridge actually builds
# from, filtered to text inputs. No hand-written file list.
function Get-InputHashes {
    $files = @(git -C $repo ls-files --cached --others --exclude-standard -- `
        'Cargo.toml' 'Cargo.lock' `
        'crates/fintwind-remote-crypto-wasm' `
        'crates/fintwind-remote-crypto' `
        'crates/fintwind-remote-protocol' `
        'crates/fintwind-protocol')
    if ($LASTEXITCODE -ne 0) { throw 'input enumeration failed' }
    $hashes = [ordered]@{}
    foreach ($file in ($files | Sort-Object -Unique)) {
        if ($file -match '\.(rs|toml|lock|yml|ps1)$') {
            $hashes[$file] = (Get-FileHash -LiteralPath (Join-Path $repo $file) -Algorithm SHA256).Hash.ToLower()
        }
    }
    return $hashes
}

# Source state is captured BEFORE any child can run.
$sourceHashes = Get-InputHashes

# Environment whitelist with full restore; the toolchain caches stay
# (CARGO_HOME/RUSTUP_HOME) but HOME and data locations are round-scoped.
$originalEnv = [ordered]@{}
foreach ($entry in [System.Environment]::GetEnvironmentVariables().GetEnumerator()) {
    $originalEnv[$entry.Key] = $entry.Value
}
$keep = @(
    'PATH', 'PATHEXT', 'COMSPEC', 'SYSTEMROOT', 'WINDIR', 'SYSTEMDRIVE',
    'TEMP', 'TMP', 'PROGRAMDATA', 'PROGRAMFILES', 'COMMONPROGRAMFILES',
    'NUMBER_OF_PROCESSORS', 'PROCESSOR_ARCHITECTURE', 'PROCESSOR_IDENTIFIER',
    'PROCESSOR_LEVEL', 'PROCESSOR_REVISION', 'CARGO_HOME', 'RUSTUP_HOME'
)
foreach ($name in @($originalEnv.Keys)) {
    if ($keep -notcontains $name) { Remove-Item -LiteralPath "env:$name" -ErrorAction SilentlyContinue }
}
foreach ($dir in @($homeDir, $tmpDir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
$env:HOME = $homeDir
$env:USERPROFILE = $homeDir
$env:APPDATA = Join-Path $homeDir 'roaming'
$env:LOCALAPPDATA = Join-Path $homeDir 'local'
$env:TEMP = $tmpDir
$env:TMP = $tmpDir
$env:CARGO_TARGET_DIR = $targetDir
$env:FINTWIND_E2E_WASM_DIR = $pkg

function Restore-Environment {
    foreach ($name in @([System.Environment]::GetEnvironmentVariables().Keys)) {
        if (-not $originalEnv.Contains($name)) { Remove-Item -LiteralPath "env:$name" -ErrorAction SilentlyContinue }
    }
    foreach ($kv in $originalEnv.GetEnumerator()) {
        [System.Environment]::SetEnvironmentVariable($kv.Key, $kv.Value)
    }
}

# Redact in memory, then write back bounded. Each stage gets its own
# output pair — nothing later in the round can overwrite an earlier
# stage's log, and no raw key material survives on disk.
function Protect-Log([string]$path) {
    if (-not (Test-Path $path)) {
        return [ordered]@{ file = (Split-Path -Leaf $path); present = $false; findings = 0; truncated = $false }
    }
    $raw = [System.IO.File]::ReadAllText($path)
    $pattern = '[A-Za-z0-9+/_-]{43,}={0,2}'
    $findings = ([regex]::Matches($raw, $pattern)).Count
    $clean = [regex]::Replace($raw, $pattern, '[REDACTED]')
    $truncated = $false
    if ($clean.Length -gt 131072) { $clean = $clean.Substring(0, 131072); $truncated = $true }
    [System.IO.File]::WriteAllText($path, $clean)
    [ordered]@{ file = (Split-Path -Leaf $path); present = $true; findings = $findings; truncated = $truncated }
}

# Children this round starts, held by their Process objects. Kill is
# transitional until the owned job-object launcher lands; containment is
# only claimed per handle, from the handle itself.
$owned = [System.Collections.Generic.List[object]]::new()
$containment = [ordered]@{
    strategy = 'transitional: owned-handle kill + exit confirmation; job object pending ownedprocesslauncher'
    children = 0
    killed = 0
    stoppedConfirmed = 0
    containmentOk = $true
}

function Start-Stage([string]$name, [string]$file, [string[]]$arguments) {
    $stub = "{0:00}-{1}" -f ($owned.Count + 1), ($name -replace '[^a-z0-9-]', '')
    $out = Join-Path $round ($stub + '.out.log')
    $err = Join-Path $round ($stub + '.err.log')
    $process = Start-Process -FilePath $file -ArgumentList $arguments `
        -WorkingDirectory $repo -NoNewWindow -PassThru `
        -RedirectStandardOutput $out -RedirectStandardError $err
    $owned.Add([pscustomobject]@{
        Name = $name
        Process = $process
        OutLog = $out
        ErrLog = $err
    })
    return $process
}

function Wait-Stage($process, [string]$name) {
    $remaining = $deadline - (Get-Date).ToUniversalTime()
    if (-not $process.WaitForExit([int][Math]::Max(1, $remaining.TotalMilliseconds))) {
        throw "stage exceeded the round deadline: $name (pid $($process.Id))"
    }
    if ($process.ExitCode -ne 0) {
        throw "stage failed (exit $($process.ExitCode)): $name (pid $($process.Id)); see its stage logs"
    }
}

$startedAt = (Get-Date).ToUniversalTime()
$deadline = $startedAt + [timespan]::FromMinutes(20)
$checks = @()
$failure = $null
$ok = $false

try {
    $checks += 'wasm-pack build --target no-modules --out-dir <round>/pkg --out-name fintwind-crypto --dev'
    Wait-Stage (Start-Stage 'wasm-pack' @(
        'wasm-pack', 'build', $crate, '--target', 'no-modules',
        '--out-dir', $pkg, '--out-name', 'fintwind-crypto', '--dev'
    )) 'wasm-pack build'

    $checks += 'cargo test --locked -p fintwind-remote-crypto-wasm --test e2e_remote_link'
    Wait-Stage (Start-Stage 'e2e-remote-link' @(
        'cargo', 'test', '--locked', '-p', 'fintwind-remote-crypto-wasm', '--test', 'e2e_remote_link'
    )) 'e2e_remote_link'

    $checks += 'cargo test --locked -p fintwind-remote-crypto-wasm --test e2e_replay_foreign'
    Wait-Stage (Start-Stage 'e2e-replay-foreign' @(
        'cargo', 'test', '--locked', '-p', 'fintwind-remote-crypto-wasm', '--test', 'e2e_replay_foreign'
    )) 'e2e_replay_foreign'

    $ok = $true
}
catch {
    # A failed round is evidence too: the summary below is written for
    # failures before the script exits non-zero.
    $failure = $_.Exception.Message
}
finally {
    foreach ($child in $owned) {
        $containment.children++
        $process = $child.Process
        if (-not $process.HasExited) {
            try {
                $process.Kill($true)
                $containment.killed++
            }
            catch {
                $containment.containmentOk = $false
                $containment["killError-$($child.Name)"] = $_.Exception.Message
            }
        }
        if ($process.HasExited -or $process.WaitForExit(10000)) {
            $containment.stoppedConfirmed++
        }
        else {
            $containment.containmentOk = $false
        }
    }
    $logAudit = @()
    foreach ($child in $owned) {
        $logAudit += (Protect-Log $child.OutLog)
        $logAudit += (Protect-Log $child.ErrLog)
    }
    Restore-Environment
}
$finishedAt = (Get-Date).ToUniversalTime()

$endHashes = Get-InputHashes
$inputsChanged = ($sourceHashes | ConvertTo-Json -Depth 10 -Compress) -ne ($endHashes | ConvertTo-Json -Depth 10 -Compress)

# Privacy from the redacted logs, computed not declared.
$logFindings = 0
foreach ($entry in $logAudit) { $logFindings += [int]$entry.findings }
$privacy = [ordered]@{
    scanned = $true
    logFindings = $logFindings
    logAudit = $logAudit
    assertNoLeak = ($ok -and ($logFindings -eq 0))
}

# Actual build outputs this round produced, hashed from disk.
$wasmBin = Join-Path $pkg 'fintwind-crypto_bg.wasm'
$wasmJs = Join-Path $pkg 'fintwind-crypto.js'
$wasmBinSha = if (Test-Path $wasmBin) { (Get-FileHash -LiteralPath $wasmBin -Algorithm SHA256).Hash.ToLower() } else { $null }
$wasmJsSha = if (Test-Path $wasmJs) { (Get-FileHash -LiteralPath $wasmJs -Algorithm SHA256).Hash.ToLower() } else { $null }

[ordered]@{
    stage = 'wasm-bridge-e2e-run'
    blocked = $false
    startedAt = $startedAt.ToString('o')
    finishedAt = $finishedAt.ToString('o')
    ok = $ok
    failure = $failure
    roundDirectory = $round
    wasmArtifact = $pkg
    wasmArtifactSha256 = [ordered]@{ 'fintwind-crypto_bg.wasm' = $wasmBinSha; 'fintwind-crypto.js' = $wasmJsSha }
    checksExecuted = $checks
    deadlineMinutes = 20
    locked = $true
    sourceHashes = $sourceHashes
    endHashes = $endHashes
    inputsChangedDuringRun = $inputsChanged
    processContainment = $containment
    privacy = $privacy
    fullPlanVerified = $false
    publicChainVerified = $false
    environment = 'whitelist with round-scoped HOME/DATA; full restore in finally'
    cleanup = 'none: prior rounds are preserved; production wasm output is never written here'
} | ConvertTo-Json -Depth 10 | Set-Content -Path $summary

$roundClean = $ok -and (-not $inputsChanged) -and $containment.containmentOk -and ($containment.stoppedConfirmed -eq $containment.children) -and $privacy.assertNoLeak
if ($roundClean) {
    Write-Host "[OK] wasm bridge e2e round: $round"
    exit 0
}
Write-Host "[X] wasm bridge e2e round failed: $round (see $summary)"
exit 1
