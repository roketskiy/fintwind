# Runs the relay quota stress: both global admission caps competing, with
# a repeatable JSON artifact. Every round writes to its own directory —
# an earlier round's artifact is evidence too, and must not be
# overwritten by a later run.
#
# STATUS: HELD. The entry below carries a no-bypass hold: until main
# lifts it (a validated owned-process launcher / job containment is
# pending), this script must not start children, alter the environment,
# or claim any verification. Everything past the hold is the repaired
# runner body — reachable only after the hold block is removed.
$ErrorActionPreference = 'Stop'

$repo = git -C $PSScriptRoot rev-parse --show-toplevel
if ($LASTEXITCODE -ne 0) { throw 'not inside a git work tree' }

$base = if ($env:FINTWIND_STRESS_OUT) { $env:FINTWIND_STRESS_OUT } else { Join-Path 'C:\Users\Public\Temp\opencode' 'fintwind-relay-stress' }
$round = Join-Path $base ("round-" + [guid]::NewGuid().ToString("N").Substring(0, 12))
New-Item -ItemType Directory -Force -Path $round | Out-Null
$summary = Join-Path $round 'run-summary.json'

# ----------------------------------------------------------------------
# NO-BYPASS HOLD — main lifts this by deleting this block. No argument
# and no environment value can skip it.
[ordered]@{
    stage = 'relay-quota-stress-run'
    blocked = $true
    blockedReason = ('runner repairs (source-hash before run, full git input enumeration, ' +
        'per-run log redaction, environment whitelist) are written but unproven, and ' +
        'process containment still relies on a handle kill rather than an owned job ' +
        'object. main lifts this hold after the ownedprocesslauncher review; there is ' +
        'no bypass.')
    processContainment = 'pending: ownedprocesslauncher (job object) — transitional handle kill only'
    fullPlanVerified = $false
    publicChainVerified = $false
    roundDirectory = $round
} | ConvertTo-Json -Depth 10 | Set-Content -Path $summary
Write-Host "[HOLD] quota stress runner blocked pending main's review: $round"
exit 2

# ----------------------------------------------------------------------
# Runner body (unreachable while the hold above stands).
$stdoutLog = Join-Path $round 'run-output.log'
$stderrLog = Join-Path $round 'run-output.err.log'
$homeDir = Join-Path $round 'home'
$tmpDir = Join-Path $round 'tmp'
$targetDir = Join-Path $round 'target'

# Full dependency enumeration, same method as main's wrapper: everything
# tracked or new-but-ignored under the crates this stress actually
# builds, filtered to text inputs. No hand-written file list.
function Get-InputHashes {
    $files = @(git -C $repo ls-files --cached --others --exclude-standard -- `
        'Cargo.toml' 'Cargo.lock' `
        'crates/fintwind-relay' `
        'crates/fintwind-remote-protocol' `
        'crates/fintwind-remote-crypto' `
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

# Source state is captured BEFORE any child can run — that ordering is
# what makes inputsChangedDuringRun mean something.
$sourceHashes = Get-InputHashes

# Environment whitelist with full restore: the child inherits only the
# keys below, plus the round-scoped variables injected afterwards; HOME
# and data locations point inside the round, never at the operator's.
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
$env:FINTWIND_STRESS_OUT = $round

function Restore-Environment {
    foreach ($name in @([System.Environment]::GetEnvironmentVariables().Keys)) {
        if (-not $originalEnv.Contains($name)) { Remove-Item -LiteralPath "env:$name" -ErrorAction SilentlyContinue }
    }
    foreach ($kv in $originalEnv.GetEnumerator()) {
        [System.Environment]::SetEnvironmentVariable($kv.Key, $kv.Value)
    }
}

# Redact in memory, then write back bounded: no raw key material or its
# encoded form survives on disk, and the summary carries counts only.
function Protect-Log([string]$path) {
    if (-not (Test-Path $path)) {
        return [ordered]@{ file = (Split-Path -Leaf $path); present = $false; findings = 0; truncated = $false }
    }
    $raw = [System.IO.File]::ReadAllText($path)
    # >=43 chars of base64-ish text is the encoding of a 32-byte key —
    # hashes get caught too, which is harmless. Everything else in the
    # log stays reviewable.
    $pattern = '[A-Za-z0-9+/_-]{43,}={0,2}'
    $findings = ([regex]::Matches($raw, $pattern)).Count
    $clean = [regex]::Replace($raw, $pattern, '[REDACTED]')
    $truncated = $false
    if ($clean.Length -gt 131072) { $clean = $clean.Substring(0, 131072); $truncated = $true }
    [System.IO.File]::WriteAllText($path, $clean)
    [ordered]@{ file = (Split-Path -Leaf $path); present = $true; findings = $findings; truncated = $truncated }
}

$startedAt = (Get-Date).ToUniversalTime()
$deadline = $startedAt + [timespan]::FromMinutes(15)
$checks = @()
$ok = $false
$failure = $null

# The one child this round starts, held by its Process object. Kill is
# transitional until the owned job-object launcher lands; containment is
# only claimed when the handle itself confirms exit.
$owned = $null
$containment = [ordered]@{
    strategy = 'transitional: owned-handle kill + exit confirmation; job object pending ownedprocesslauncher'
    killed = 0
    stoppedConfirmed = 0
    containmentOk = $true
}
try {
    $checks += 'cargo test --locked -p fintwind-relay --test quota_stress -- --test-threads=1 --nocapture'
    $owned = Start-Process -FilePath 'cargo' `
        -ArgumentList @('test', '--locked', '-p', 'fintwind-relay', '--test', 'quota_stress', '--', '--test-threads=1', '--nocapture') `
        -WorkingDirectory $repo -NoNewWindow -PassThru `
        -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog
    $remaining = $deadline - (Get-Date).ToUniversalTime()
    if (-not $owned.WaitForExit([int][Math]::Max(1, $remaining.TotalMilliseconds))) {
        throw 'the quota stress exceeded the round deadline (15 minutes)'
    }
    if ($owned.ExitCode -ne 0) {
        throw "quota stress failed (exit $($owned.ExitCode)); see run-output logs"
    }
    $ok = $true
}
catch {
    # A failed round is evidence too: the summary below is written for
    # failures before the script exits non-zero.
    $failure = $_.Exception.Message
}
finally {
    if ($owned) {
        if (-not $owned.HasExited) {
            try {
                $owned.Kill($true)
                $containment.killed = 1
            }
            catch {
                $containment.containmentOk = $false
                $containment.killError = $_.Exception.Message
            }
        }
        if ($owned.HasExited -or $owned.WaitForExit(10000)) {
            $containment.stoppedConfirmed = 1
        }
        else {
            $containment.containmentOk = $false
            $containment.stoppedConfirmed = 0
        }
    }
    $logAudit = @()
    foreach ($log in @($stdoutLog, $stderrLog)) { $logAudit += (Protect-Log $log) }
    Restore-Environment
}
$finishedAt = (Get-Date).ToUniversalTime()

$endHashes = Get-InputHashes
$inputsChanged = ($sourceHashes | ConvertTo-Json -Depth 10 -Compress) -ne ($endHashes | ConvertTo-Json -Depth 10 -Compress)

# Privacy is computed, never declared: no artifact scan or any finding
# in the redacted logs means the claim may be true; anything else fails
# the round.
$artifactPath = Join-Path $round 'relay-quota-stress.json'
$artifactFindings = $null
if (Test-Path $artifactPath) {
    $rawArtifact = [System.IO.File]::ReadAllText($artifactPath)
    $artifactFindings = ([regex]::Matches($rawArtifact, '[A-Za-z0-9+/_-]{43,}={0,2}')).Count
}
$logFindings = 0
foreach ($entry in $logAudit) { $logFindings += [int]$entry.findings }
$privacy = [ordered]@{
    scanned = ($null -ne $artifactFindings)
    artifactFindings = $artifactFindings
    logFindings = $logFindings
    logAudit = $logAudit
    assertNoLeak = ($ok -and ($null -ne $artifactFindings) -and ($artifactFindings -eq 0) -and ($logFindings -eq 0))
}

# Actual build outputs this round produced, hashed from disk.
$relayBin = Join-Path $targetDir 'debug\fintwind-relay.exe'
$testRunner = Join-Path $targetDir 'debug\quota_stress.exe'
$relayBinSha = if (Test-Path $relayBin) { (Get-FileHash -LiteralPath $relayBin -Algorithm SHA256).Hash.ToLower() } else { $null }
$runnerSha = if (Test-Path $testRunner) { (Get-FileHash -LiteralPath $testRunner -Algorithm SHA256).Hash.ToLower() } else { $null }

[ordered]@{
    stage = 'relay-quota-stress-run'
    blocked = $false
    startedAt = $startedAt.ToString('o')
    finishedAt = $finishedAt.ToString('o')
    ok = $ok
    failure = $failure
    roundDirectory = $round
    checksExecuted = $checks
    deadlineMinutes = 15
    locked = $true
    sourceHashes = $sourceHashes
    endHashes = $endHashes
    inputsChangedDuringRun = $inputsChanged
    binarySha256 = $relayBinSha
    runnerSha256 = $runnerSha
    processContainment = $containment
    privacy = $privacy
    fullPlanVerified = $false
    publicChainVerified = $false
    environment = 'whitelist with round-scoped HOME/DATA; full restore in finally'
    cleanup = 'none: prior rounds are preserved'
} | ConvertTo-Json -Depth 10 | Set-Content -Path $summary

$roundClean = $ok -and (-not $inputsChanged) -and $containment.containmentOk -and ($containment.stoppedConfirmed -eq 1) -and $privacy.assertNoLeak
if ($roundClean) {
    Write-Host "[OK] quota stress round: $round"
    exit 0
}
Write-Host "[X] quota stress round failed: $round (see $summary)"
exit 1
