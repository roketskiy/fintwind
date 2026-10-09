# check-remote-trust.ps1 - out-of-process black box for the remote-host trust
# surface. Drives the real `fintwind-remote-host.exe` binary; asserts on the
# filesystem and process exits it produces. No unit tests, no in-process API
# calls, no user data: every directory is a fresh unique path under
# C:/Users/Public/Temp/opencode, and the run directory carries a GUID so two
# runs can never overwrite each other.
#
# Failure scenarios this script refuses to let pass (kept in sync with
# crates/fintwind-remote-host/src/identity.rs):
#   fresh-identity        a fresh identity must be durable or fail; repeated
#                         loads must not drift
#   legacy-migration      migration must keep the public key and host id,
#                         seal both records, delete the plaintext only after
#                         a verified round trip
#   corrupted-sealed      a corrupted sealed record is fatal, and a planted
#                         plaintext beside it is NOT adopted
#   migration-write-fail  a blocked migration target keeps the (already
#                         on-disk) legacy plaintext and the identity loads
#   sealed-wins           a planted plaintext never replaces a sealed record
#   fresh-write-fail      a fresh generation whose seal target is occupied
#                         fails nonzero: no plaintext, no grant, no
#                         registration, nothing half-done
#   sealed-unreadable     a sealed record that cannot be read (held with no
#                         sharing) is fatal even with a planted plaintext
#   mixed-half-identity   one sealed key without the other is refused, never
#                         completed with a fresh half
#   fresh-concurrent      two processes generating on an empty directory end
#                         with one identity, both loads agreeing
#   legacy-concurrent     two migrations of the same plaintext never tear a
#                         record
#   revoke-local-first    a registered host with an unreachable relay still
#                         revokes locally and keeps the audit row
#   fresh-acl-deny        a directory that denies file creation fails fresh
#                         generation cleanly (skipped when the ACL cannot be
#                         set on this machine)
#   lock-recovery        a stale lock filename is not a live lock; a live
#                         OS lock remains exclusive regardless of its age
#   blocked-second-key   a failed fresh second write never registers or
#                         fills its stored half with a different generation
#
# The artifact records names, hashes, booleans and phases only. Private
# keys, nonces, URLs, stderr and raw error text never reach it. A passing
# check records no detail text; only a failure records its reason.

param(
    [string]$ArtifactDirectory = '',
    [string]$HostBinary = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$host_exe = Join-Path (Join-Path (Join-Path $repo 'target') 'debug') 'fintwind-remote-host.exe'
if ($HostBinary) { $host_exe = [System.IO.Path]::GetFullPath($HostBinary) }
if (-not (Test-Path $host_exe)) {
    throw "fintwind-remote-host.exe not found at $host_exe; build it first."
}
$guid = [guid]::NewGuid().ToString('N').Substring(0, 8)
if (-not $ArtifactDirectory) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $ArtifactDirectory = Join-Path 'C:/Users/Public/Temp/opencode' "remote-trust-$stamp-$guid"
} else {
    $ArtifactDirectory = "$ArtifactDirectory-$guid"
}
$null = New-Item -ItemType Directory -Path $ArtifactDirectory -Force

$checks = [System.Collections.Generic.List[object]]::new()
$evidence = @{}
$phase = 'preflight'
$processes = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$isolatedHome = Join-Path $ArtifactDirectory 'isolated-home'
$null = New-Item -ItemType Directory -Path $isolatedHome

function Start-HostTool {
    param([string[]]$Arguments, [string]$DataDir)
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $script:host_exe
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $DataDir
    foreach ($argument in @($Arguments) + @('--data-dir', $DataDir)) { $start.ArgumentList.Add($argument) }
    $start.Environment.Clear()
    foreach ($name in @('PATH', 'PATHEXT', 'SystemRoot', 'WINDIR', 'ComSpec', 'SYSTEMDRIVE')) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if ($value) { $start.Environment[$name] = $value }
    }
    foreach ($name in @('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'XDG_CONFIG_HOME', 'TEMP', 'TMP')) {
        $start.Environment[$name] = $script:isolatedHome
    }
    $process = [System.Diagnostics.Process]::Start($start)
    $script:processes.Add($process)
    return @{ Process = $process; Output = $process.StandardOutput.ReadToEndAsync(); Error = $process.StandardError.ReadToEndAsync() }
}

function Complete-HostTool {
    param($Invocation)
    if (-not $Invocation.Process.WaitForExit(30000)) {
        $Invocation.Process.Kill($true)
        $Invocation.Process.WaitForExit()
        throw 'A trust subprocess exceeded its deadline.'
    }
    $null = $Invocation.Error.GetAwaiter().GetResult()
    return @{ ExitCode = $Invocation.Process.ExitCode; Output = $Invocation.Output.GetAwaiter().GetResult() }
}

function Add-Check {
    param([string]$Name, [bool]$Passed, [string]$FailReason = '')
    $script:checks.Add([ordered]@{
        name = $Name
        passed = $Passed
        detail = $(if ($Passed) { '' } else { $FailReason })
        phase = $script:phase
    })
    if (-not $Passed) { throw "Check failed: $Name ($FailReason)" }
}

function New-CaseDir {
    param([string]$Name)
    $dir = Join-Path $ArtifactDirectory $Name
    $null = New-Item -ItemType Directory -Path $dir -Force
    return $dir
}

function Invoke-HostTool {
    param([string[]]$Arguments, [string]$DataDir)
    return Complete-HostTool (Start-HostTool -Arguments $Arguments -DataDir $DataDir)
}

function Get-Identity {
    param([string]$DataDir)
    $result = Invoke-HostTool -Arguments @('--print-identity') -DataDir $DataDir
    if ($result.ExitCode -ne 0) { return $null }
    try { return $result.Output | ConvertFrom-Json } catch { return $null }
}

function Get-FileHashSafe {
    param([string]$Path)
    if (Test-Path $Path) {
        return (Get-FileHash -Path $Path -Algorithm SHA256).Hash.ToLower()
    }
    return $null
}

# A stable digest of a public key: the artifact records this, never the key.
function Get-KeyDigest {
    param([string]$Base64Key)
    if ([string]::IsNullOrEmpty($Base64Key)) { return $null }
    $bytes = [Convert]::FromBase64String($Base64Key)
    $hash = [System.Security.Cryptography.SHA256]::Create().ComputeHash($bytes)
    return ($hash | ForEach-Object { $_.ToString('x2') }) -join ''
}

function New-RandomBytes {
    param([int]$Count)
    $bytes = [byte[]]::new($Count)
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $rng.GetBytes($bytes)
    return $bytes
}

# Source inputs a reviewer needs to tie the artifact to the code state: the
# trust surface, the runner itself, and the crate inputs.
$sourceHashes = [ordered]@{}
foreach ($relative in @(
    'crates/fintwind-remote-host/src/identity.rs',
    'crates/fintwind-remote-host/src/identity/dpapi.rs',
    'crates/fintwind-remote-host/src/grants.rs',
    'crates/fintwind-remote-host/src/pair.rs',
    'crates/fintwind-remote-host/Cargo.toml',
    'Cargo.lock',
    'scripts/check-remote-trust.ps1'
)) {
    $sourceHashes[$relative] = Get-FileHashSafe (Join-Path $repo $relative)
}

try {
    # ---- fresh identity, then a repeated load -----------------------------
    $phase = 'fresh-identity'
    $dir = New-CaseDir 'fresh'
    $first = Get-Identity -DataDir $dir
    Add-Check 'fresh identity prints keys' ($null -ne $first) 'no identity output'
    Add-Check 'fresh host id is empty' ($first.hostId -eq '') 'the host id was not empty'
    Add-Check 'sealed signing record exists' (Test-Path "$dir\host-signing-key.dpapi") 'missing host-signing-key.dpapi'
    Add-Check 'sealed noise record exists' (Test-Path "$dir\host-noise-key.dpapi") 'missing host-noise-key.dpapi'
    Add-Check 'no plaintext signing secret' (-not (Test-Path "$dir\host-signing-key.bin")) 'plaintext host-signing-key.bin exists'
    Add-Check 'no plaintext noise secret' (-not (Test-Path "$dir\host-noise-key.bin")) 'plaintext host-noise-key.bin exists'
    $second = Get-Identity -DataDir $dir
    Add-Check 'repeat load keeps the noise key' ($second.noisePublicKey -eq $first.noisePublicKey) 'the noise key drifted'
    Add-Check 'repeat load keeps the signing key' ($second.signingPublicKey -eq $first.signingPublicKey) 'the signing key drifted'
    $evidence['fresh'] = [ordered]@{
        noiseKeyDigest = Get-KeyDigest $first.noisePublicKey
        signingKeyDigest = Get-KeyDigest $first.signingPublicKey
        identityReloaded = ($second.noisePublicKey -eq $first.noisePublicKey)
    }

    # ---- legacy plaintext migrates without changing the identity ---------
    $phase = 'legacy-migration'
    $dir = New-CaseDir 'legacy'
    $signingBytes = New-RandomBytes 32
    $noiseBytes = New-RandomBytes 64
    [System.IO.File]::WriteAllBytes("$dir\host-signing-key.bin", $signingBytes)
    [System.IO.File]::WriteAllBytes("$dir\host-noise-key.bin", $noiseBytes)
    $expectedNoise = [Convert]::ToBase64String($noiseBytes[0..31])
    $hostIdText = [guid]::NewGuid().ToString()
    [System.IO.File]::WriteAllText("$dir\host-id.txt", $hostIdText)
    $migrated = Get-Identity -DataDir $dir
    Add-Check 'legacy identity still loads' ($null -ne $migrated) 'the load failed'
    Add-Check 'migration keeps the noise public key' ($migrated.noisePublicKey -eq $expectedNoise) 'the noise public key changed'
    Add-Check 'migration keeps the host id' ($migrated.hostId -eq $hostIdText) 'the host id changed'
    Add-Check 'migration sealed the signing key' (Test-Path "$dir\host-signing-key.dpapi") 'no sealed signing record'
    Add-Check 'migration sealed the noise key' (Test-Path "$dir\host-noise-key.dpapi") 'no sealed noise record'
    Add-Check 'migration deleted the plaintext signing secret' (-not (Test-Path "$dir\host-signing-key.bin")) 'the plaintext signing secret survived'
    Add-Check 'migration deleted the plaintext noise secret' (-not (Test-Path "$dir\host-noise-key.bin")) 'the plaintext noise secret survived'
    $again = Get-Identity -DataDir $dir
    Add-Check 'sealed record reloads identically' ($again.noisePublicKey -eq $expectedNoise) 'the sealed record drifted'
    $evidence['legacy'] = [ordered]@{
        sealedNoiseSha256 = Get-FileHashSafe "$dir\host-noise-key.dpapi"
        sealedSigningSha256 = Get-FileHashSafe "$dir\host-signing-key.dpapi"
        plaintextNoiseRemoved = -not (Test-Path "$dir\host-noise-key.bin")
        plaintextSigningRemoved = -not (Test-Path "$dir\host-signing-key.bin")
        hostIdKept = ($migrated.hostId -eq $hostIdText)
    }

    # ---- a corrupted sealed record never becomes a new identity ----------
    # With a planted plaintext beside it: the sealed record is still the
    # identity, the load fails, and the planted file is left untouched.
    $phase = 'corrupted-sealed'
    $dir = New-CaseDir 'corrupted'
    $null = Get-Identity -DataDir $dir | Out-Null
    $sealedNoiseBefore = Get-FileHashSafe "$dir\host-noise-key.dpapi"
    $planted = New-RandomBytes 64
    [System.IO.File]::WriteAllBytes("$dir\host-noise-key.bin", $planted)
    [System.IO.File]::WriteAllBytes("$dir\host-noise-key.dpapi", [byte[]]::new(64))
    $corrupt = Invoke-HostTool -Arguments @('--print-identity') -DataDir $dir
    Add-Check 'corrupted sealed record fails the tool' ($corrupt.ExitCode -ne 0) 'the tool accepted a corrupt record'
    Add-Check 'corrupted record does not mint a plaintext secret' (-not (Test-Path "$dir\host-signing-key.bin")) 'a plaintext signing secret appeared'
    Add-Check 'planted plaintext is not adopted' ($corrupt.ExitCode -ne 0 -and (Test-Path "$dir\host-noise-key.bin")) 'the planted plaintext was consumed or removed'
    $evidence['corrupted'] = [ordered]@{
        sealedNoiseRewritten = (Get-FileHashSafe "$dir\host-noise-key.dpapi") -ne $sealedNoiseBefore
    }

    # ---- a blocked migration target keeps the legacy plaintext -----------
    # The historical compatibility case: plaintext identity already on
    # disk, the sealed target name occupied by a directory. The host keeps
    # running on the identity it has.
    $phase = 'migration-write-fail'
    $dir = New-CaseDir 'write-failure'
    $signingBytes = New-RandomBytes 32
    $noiseBytes = New-RandomBytes 64
    [System.IO.File]::WriteAllBytes("$dir\host-signing-key.bin", $signingBytes)
    [System.IO.File]::WriteAllBytes("$dir\host-noise-key.bin", $noiseBytes)
    $null = New-Item -ItemType Directory -Path "$dir\host-signing-key.dpapi" -Force
    $null = New-Item -ItemType Directory -Path "$dir\host-noise-key.dpapi" -Force
    $failed = Invoke-HostTool -Arguments @('--print-identity') -DataDir $dir
    Add-Check 'identity still loads when sealing fails' ($failed.ExitCode -eq 0) 'the tool refused a loadable identity'
    $loaded = $null
    try { $loaded = $failed.Output | ConvertFrom-Json } catch {}
    Add-Check 'identity is the plaintext one' ($null -ne $loaded -and $loaded.noisePublicKey -eq [Convert]::ToBase64String($noiseBytes[0..31])) 'the identity changed on a failed seal'
    Add-Check 'plaintext signing secret survives' (Test-Path "$dir\host-signing-key.bin") 'the plaintext secret was deleted'
    Add-Check 'plaintext noise secret survives' (Test-Path "$dir\host-noise-key.bin") 'the plaintext secret was deleted'
    Add-Check 'no temp record is left behind' (-not (Test-Path "$dir\host-signing-key.dpapi.tmp*") -and -not (Get-ChildItem "$dir" -Filter '*.tmp-*' -ErrorAction SilentlyContinue)) 'a temp record survived the failed write'

    # ---- a planted plaintext never replaces a sealed identity ------------
    $phase = 'sealed-wins'
    $dir = New-CaseDir 'sealed-wins'
    $sealed = Get-Identity -DataDir $dir
    [System.IO.File]::WriteAllBytes("$dir\host-noise-key.bin", (New-RandomBytes 64))
    $after = Get-Identity -DataDir $dir
    Add-Check 'sealed record wins over a planted plaintext' ($after.noisePublicKey -eq $sealed.noisePublicKey) 'the planted plaintext replaced the identity'

    # ---- a fresh generation with an occupied seal target fails clean -----
    # No plaintext fallback for a fresh identity: if it cannot be durably
    # stored, the tool fails and leaves nothing behind.
    $phase = 'fresh-write-fail'
    $dir = New-CaseDir 'fresh-write-fail'
    $null = New-Item -ItemType Directory -Path "$dir\host-signing-key.dpapi" -Force
    $null = New-Item -ItemType Directory -Path "$dir\host-noise-key.dpapi" -Force
    $freshFail = Invoke-HostTool -Arguments @('--print-identity') -DataDir $dir
    Add-Check 'fresh generation with a blocked seal target fails' ($freshFail.ExitCode -ne 0) 'the tool minted an unstorable identity anyway'
    Add-Check 'no plaintext secret was written' (-not (Test-Path "$dir\host-signing-key.bin") -and -not (Test-Path "$dir\host-noise-key.bin")) 'a plaintext secret appeared'
    Add-Check 'no grant appeared' (-not (Test-Path "$dir\paired-device.json")) 'a grant file appeared'
    Add-Check 'no registration appeared' (-not (Test-Path "$dir\host-id.txt")) 'a host id appeared'
    Add-Check 'the blocked target remains unchanged' ((Get-Item "$dir\host-signing-key.dpapi").PSIsContainer) 'the failed generation overwrote the target'

    # ---- an unreadable sealed record is fatal, plaintext or not ----------
    # The sealed record is held open with no sharing, so the child cannot
    # read it: the load must fail closed and never adopt the planted
    # plaintext beside it.
    $phase = 'sealed-unreadable'
    $dir = New-CaseDir 'sealed-unreadable'
    $unreadable = Get-Identity -DataDir $dir
    [System.IO.File]::WriteAllBytes("$dir\host-noise-key.bin", (New-RandomBytes 64))
    $held = [System.IO.File]::Open("$dir\host-noise-key.dpapi", 'Open', 'Read', 'None')
    try {
        $locked = Invoke-HostTool -Arguments @('--print-identity') -DataDir $dir
    } finally {
        $held.Close()
    }
    Add-Check 'unreadable sealed record fails the tool' ($locked.ExitCode -ne 0) 'the tool accepted an unreadable record'
    $unlocked = Get-Identity -DataDir $dir
    Add-Check 'the identity survives after the reader lets go' ($null -ne $unlocked -and $unlocked.noisePublicKey -eq $unreadable.noisePublicKey) 'the identity changed after the lock'
    Add-Check 'the planted plaintext was never adopted' ($unlocked.noisePublicKey -eq $unreadable.noisePublicKey) 'the planted plaintext replaced the identity'

    # ---- a half-stored identity is refused, never completed --------------
    $phase = 'mixed-half-identity'
    $dir = New-CaseDir 'mixed-half'
    $source = New-CaseDir 'mixed-half-source'
    $null = Get-Identity -DataDir $source | Out-Null
    Copy-Item "$source\host-signing-key.dpapi" "$dir\host-signing-key.dpapi"
    $half = Invoke-HostTool -Arguments @('--print-identity') -DataDir $dir
    Add-Check 'a half identity is refused' ($half.ExitCode -ne 0) 'the tool completed a half identity'
    Add-Check 'no noise record was minted for the half' (-not (Test-Path "$dir\host-noise-key.dpapi") -and -not (Test-Path "$dir\host-noise-key.bin")) 'a fresh noise half appeared'

    # ---- two fresh generations race: one identity, both agree ------------
    $phase = 'fresh-concurrent'
    $dir = New-CaseDir 'fresh-concurrent'
    $jobs = @(1..2 | ForEach-Object { Start-HostTool -Arguments @('--print-identity') -DataDir $dir })
    $results = @($jobs | ForEach-Object { Complete-HostTool $_ })
    Add-Check 'both racing fresh loads exit successfully' (@($results | Where-Object { $_.ExitCode -eq 0 }).Count -eq 2) 'a racing fresh load failed'
    $raced = @($results | ForEach-Object { $_.Output })
    $racedIdentities = @($raced | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json })
    Add-Check 'both racing fresh loads succeeded' ($racedIdentities.Count -eq 2) 'a racing fresh load failed'
    Add-Check 'both racing fresh loads agree' ($racedIdentities[0].noisePublicKey -eq $racedIdentities[1].noisePublicKey -and $racedIdentities[0].signingPublicKey -eq $racedIdentities[1].signingPublicKey) 'the racers hold different identities'
    Add-Check 'no mixed plaintext key pair' (-not (Test-Path "$dir\host-signing-key.bin") -and -not (Test-Path "$dir\host-noise-key.bin")) 'the race left plaintext keys'
    $third = Get-Identity -DataDir $dir
    Add-Check 'a third load reads the raced identity' ($third.noisePublicKey -eq $racedIdentities[0].noisePublicKey) 'the raced record does not reload'

    $phase = 'lock-recovery'
    $dir = New-CaseDir 'lock-recovery'
    $lockPath = Join-Path $dir 'host-identity.lock'
    [System.IO.File]::WriteAllText($lockPath, '')
    [System.IO.File]::SetLastWriteTimeUtc($lockPath, [DateTime]::UtcNow.AddMinutes(-5))
    $recovered = Get-Identity -DataDir $dir
    Add-Check 'an abandoned lock filename does not block identity loading' ($null -ne $recovered) 'the stale filename blocked loading'
    $lockHandle = [System.IO.File]::Open($lockPath, 'Open', 'ReadWrite', 'ReadWrite')
    $lockHandle.Lock(0, 1)
    try {
        $busy = Invoke-HostTool -Arguments @('--print-identity') -DataDir $dir
        Add-Check 'a live old lock cannot be stolen' ($busy.ExitCode -ne 0) 'the live lock was ignored or stolen'
    } finally {
        $lockHandle.Unlock(0, 1)
        $lockHandle.Dispose()
    }
    $afterLock = Get-Identity -DataDir $dir
    Add-Check 'identity loading resumes after the live lock is released' ($null -ne $afterLock -and $afterLock.noisePublicKey -eq $recovered.noisePublicKey) 'the identity changed or the lock leaked'

    $phase = 'blocked-second-key'
    $dir = New-CaseDir 'blocked-second-key'
    $null = New-Item -ItemType Directory -Path (Join-Path $dir 'host-noise-key.dpapi')
    $blockedSecond = Invoke-HostTool -Arguments @('--print-identity') -DataDir $dir
    Add-Check 'a failed fresh second write refuses the identity' ($blockedSecond.ExitCode -ne 0) 'the partial identity was accepted'
    $storedHalf = Get-FileHashSafe (Join-Path $dir 'host-signing-key.dpapi')
    Remove-Item -LiteralPath (Join-Path $dir 'host-noise-key.dpapi')
    $blockedReload = Invoke-HostTool -Arguments @('--print-identity') -DataDir $dir
    Add-Check 'a failed generation never fills its half with a new key' ($blockedReload.ExitCode -ne 0 -and -not (Test-Path (Join-Path $dir 'host-noise-key.dpapi'))) 'the stored half was completed'
    Add-Check 'the stored half remains unchanged' ((Get-FileHashSafe (Join-Path $dir 'host-signing-key.dpapi')) -eq $storedHalf) 'the stored half was replaced'
    Add-Check 'the failed generation never registered or granted access' (-not (Test-Path (Join-Path $dir 'host-id.txt')) -and -not (Test-Path (Join-Path $dir 'paired-device.json'))) 'registration or a grant appeared'

    # ---- two legacy migrations of the same plaintext race ----------------
    $phase = 'legacy-concurrent'
    $dir = New-CaseDir 'legacy-concurrent'
    $noiseBytes = New-RandomBytes 64
    [System.IO.File]::WriteAllBytes("$dir\host-noise-key.bin", $noiseBytes)
    [System.IO.File]::WriteAllBytes("$dir\host-signing-key.bin", (New-RandomBytes 32))
    $expectedNoise = [Convert]::ToBase64String($noiseBytes[0..31])
    $jobs = @(1..2 | ForEach-Object { Start-HostTool -Arguments @('--print-identity') -DataDir $dir })
    $results = @($jobs | ForEach-Object { Complete-HostTool $_ })
    $raced = @($results | ForEach-Object { $_.Output })
    Add-Check 'both racing migrations keep the identity' (@($raced | Where-Object { $_ -and ($_ | ConvertFrom-Json).noisePublicKey -eq $expectedNoise }).Count -eq 2) 'a racing migration saw a different identity'
    Add-Check 'the raced migration sealed the record' (Test-Path "$dir\host-noise-key.dpapi") 'no sealed record after the race'
    $after = Get-Identity -DataDir $dir
    Add-Check 'the sealed record still opens after the race' ($after.noisePublicKey -eq $expectedNoise) 'the race tore the record'

    # ---- revoke is local-first even when the relay is unreachable --------
    # A registered host (host id present) whose relay is unreachable must
    # still cut the device off locally; an unregistered host bails before
    # the local revoke, so registration state is part of the scenario.
    $phase = 'revoke-local-first'
    $dir = New-CaseDir 'revoke'
    $null = Get-Identity -DataDir $dir | Out-Null
    [System.IO.File]::WriteAllText("$dir\host-id.txt", [guid]::NewGuid().ToString())
    $deviceId = [guid]::NewGuid().ToString()
    $deviceKey = New-RandomBytes 32
    $provision = Invoke-HostTool -Arguments @('--provision-device', "$deviceId,$([Convert]::ToBase64String($deviceKey))") -DataDir $dir
    Add-Check 'provision writes a grant' (($provision.ExitCode -eq 0) -and (Test-Path "$dir\paired-device.json")) 'provision failed'
    $revoke = Invoke-HostTool -Arguments @('--revoke-device', $deviceId, '--relay-origin', 'http://127.0.0.1:1') -DataDir $dir
    Add-Check 'revoke reports the unconfirmed relay' ($revoke.ExitCode -ne 0) 'the tool claimed a full revoke'
    $grant = Get-Content "$dir\paired-device.json" -Raw | ConvertFrom-Json
    $row = $grant.grants | Where-Object { $_.deviceId -eq $deviceId }
    Add-Check 'local grant is revoked' ($null -ne $row.revokedAtMs) 'revokedAtMs missing after a local-first revoke'
    Add-Check 'revoked grant row is kept for audit' ($null -ne $row) 'the audit row disappeared'
    $evidence['revoke'] = [ordered]@{
        revocationReported = ($revoke.ExitCode -ne 0)
        auditRowKept = ($null -ne $row)
        revokedAtMsPresent = ($null -ne $row.revokedAtMs)
    }

    # ---- a directory that denies creation fails fresh generation ---------
    # Real ACL scenario. Skipped (recorded as skipped, not failed) when the
    # deny cannot be applied on this machine. Never production keys: the
    # directory belongs to this run.
    $phase = 'fresh-acl-deny'
    $dir = New-CaseDir 'acl-deny'
    $denyApplied = $false
    try {
        $null = icacls $dir /deny ('*' + [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value + ':(WD)') 2>&1
        $denyApplied = ($LASTEXITCODE -eq 0)
    } catch {
        $denyApplied = $false
    }
    if ($denyApplied) {
        try {
            $denied = Invoke-HostTool -Arguments @('--print-identity') -DataDir $dir
            Add-Check 'a write-denied directory fails fresh generation' ($denied.ExitCode -ne 0) 'the tool stored an identity in a write-denied directory'
            Add-Check 'nothing was stored in the denied directory' ((Get-ChildItem $dir -Force | Measure-Object).Count -eq 0) 'the denied directory was written to'
        }
        finally {
            $null = icacls $dir /remove:d ('*' + [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value) 2>&1
        }
        $evidence['acl'] = [ordered]@{ applied = $true; failedClosed = ($denied.ExitCode -ne 0) }
    } else {
        $evidence['acl'] = [ordered]@{ applied = $false; skipped = 'the ACL deny could not be created on this machine' }
        Write-Host '[SKIP] acl-deny: the ACL deny could not be created.'
    }

    $report = [ordered]@{
        capturedAt = [DateTimeOffset]::UtcNow.ToString('o')
        runGuid = $guid
        binary = (Split-Path -Leaf $host_exe)
        binarySha256 = (Get-FileHash -Path $host_exe -Algorithm SHA256).Hash.ToLower()
        sourceSha256 = $sourceHashes
        checks = @($checks.ToArray())
        evidence = $evidence
        passed = $true
        publicChainVerified = $false
        note = 'Out-of-process trust black box: one real binary, fresh unique directories under C:/Users/Public/Temp/opencode, hashes/booleans/phases only. The pairing accept path is covered by the host crate pairing_loop integration test; the reject path and the live relay link are not covered here.'
    }
}
catch {
    $report = [ordered]@{
        capturedAt = [DateTimeOffset]::UtcNow.ToString('o')
        runGuid = $guid
        binary = (Split-Path -Leaf $host_exe)
        binarySha256 = (Get-FileHash -Path $host_exe -Algorithm SHA256).Hash.ToLower()
        sourceSha256 = $sourceHashes
        checks = @($checks.ToArray())
        evidence = $evidence
        passed = $false
        failedPhase = $phase
        publicChainVerified = $false
        note = 'Out-of-process trust black box: one real binary, fresh unique directories under C:/Users/Public/Temp/opencode, hashes/booleans/phases only. The pairing accept path is covered by the host crate pairing_loop integration test; the reject path and the live relay link are not covered here.'
    }
    Write-Error -ErrorAction Continue "[X] Remote-trust black box failed at $phase."
}
finally {
    foreach ($process in $processes) {
        if (-not $process.HasExited) { $process.Kill($true); $process.WaitForExit() }
    }
    $report.allOwnedProcessesStopped = @($processes | Where-Object { -not $_.HasExited }).Count -eq 0
    $reportPath = Join-Path $ArtifactDirectory 'summary.json'
    $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reportPath -Encoding utf8
    Write-Host "[ARTIFACT] $reportPath"
}

if (-not $report.passed) { exit 1 }
Write-Host '[OK] Remote-trust black box passed.'
