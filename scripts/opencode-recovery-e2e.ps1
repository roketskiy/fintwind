<#
.SYNOPSIS
    Run the OpenCode private-serve recovery E2E fault suite for Fintwind.

.DESCRIPTION
    Drives the real fintwind-core daemon + serve WebSocket + fintwind-client
    against a local fake `opencode serve` (pure loopback, no network, no real
    OpenCode service, no model requests). The fast matrix + projection + P1
    guard are the default `cargo test`. The separate suites are
    #[ignore]d and run only with -Long, each in its own daemon process, and each
    reported separately; any failed suite returns a nonzero exit code.

    Each run writes a JSON artifact (scenarios, pass/fail, request facts, event
    timelines) to temp/recovery-e2e/<uuid>/. No credentials are written.

.PARAMETER Long
    Also run the ignored suites: the ~15s idle-reconciliation matrix, the P1
    attachment-only guard, the server-initiated continuation, and
    the template-expansion case.
#>
[CmdletBinding()]
param(
    [switch]$Long
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location -Path $repo

$test = Join-Path $repo 'crates/fintwind-core/tests/opencode_recovery.rs'
if (-not (Test-Path $test)) {
    Write-Host '[X] recovery test not found: crates\fintwind-core\tests\opencode_recovery.rs'
    exit 1
}

$common = @('test', '--locked', '-p', 'fintwind-core', '--test', 'opencode_recovery')

Write-Host '[..] fast recovery E2E (discovery / fragmented SSE / reconnect / active-veto / prev-turn / R1 steer + projection + P1 guard)'
& cargo @common '--' '--nocapture'
if ($LASTEXITCODE -ne 0) {
    Write-Host '[X] fast recovery E2E failed'
    exit $LASTEXITCODE
}
Write-Host '[OK] fast recovery E2E passed'

if ($Long) {
    Write-Host '[..] long-delay idle reconciliation (waits out ~15s intervals, ~90s; must pass)'
    & cargo @common opencode_recovery_idle_reconciliation '--' '--ignored' '--nocapture'
    if ($LASTEXITCODE -ne 0) {
        Write-Host '[X] long idle-reconciliation suite failed'
        exit $LASTEXITCODE
    }
    Write-Host '[OK] long idle-reconciliation suite passed'

    Write-Host '[..] P1 attachment-only prompt (recovery translate guard)'
    & cargo @common opencode_recovery_attachment_only_prompt '--' '--ignored' '--nocapture'
    if ($LASTEXITCODE -ne 0) {
        Write-Host '[X] P1 attachment-only prompt is failing (recovery translate gap)'
        exit $LASTEXITCODE
    } else {
        Write-Host '[OK] P1 attachment-only prompt passed'
    }

    Write-Host '[..] server-initiated continuation with lost content'
    & cargo @common opencode_recovery_server_initiated_continuation '--' '--ignored' '--nocapture'
    if ($LASTEXITCODE -ne 0) {
        Write-Host '[X] server-initiated continuation failed'
        exit $LASTEXITCODE
    } else {
        Write-Host '[OK] R2 server-initiated continuation passed'
    }

    Write-Host '[..] template command expansion preserves typed input'
    & cargo @common opencode_recovery_template_expansion_preserves_typed '--' '--ignored' '--nocapture'
    if ($LASTEXITCODE -ne 0) {
        Write-Host '[X] template command recovery failed'
        exit $LASTEXITCODE
    } else {
        Write-Host '[OK] P2 template expansion passed'
    }
}

$artifact = Get-ChildItem -Path (Join-Path 'temp' 'recovery-e2e') -Recurse -Filter 'result*.json' -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
if ($artifact) {
    Write-Host ('[OK] newest artifact: ' + $artifact.FullName)
}
