param([string]$OpenCodeBinary = '')
$ErrorActionPreference = 'Stop'
# This subprocess is a credential-free regression runner. Opt-in live-model
# checks use separate scripts; never let their switches reach this suite.
Remove-Item Env:FINTWIND_AGENT_REAL_PROVIDER -ErrorAction SilentlyContinue
Remove-Item Env:DEEPSEEK_API_KEY -ErrorAction SilentlyContinue
Remove-Item Env:OPENAI_API_KEY -ErrorAction SilentlyContinue
Remove-Item Env:ANTHROPIC_API_KEY -ErrorAction SilentlyContinue
$root = Split-Path -Parent $PSScriptRoot
$module = Join-Path $root 'fintwind-go'
$output = Join-Path $root 'target/go-backend'
New-Item -ItemType Directory -Path $output -Force | Out-Null
$env:HTTP_PROXY = 'http://127.0.0.1:7897'
$env:HTTPS_PROXY = 'http://127.0.0.1:7897'
Push-Location $root
try {
    python scripts/generate-go-backend-schema.py --check
    if ($LASTEXITCODE -ne 0) { throw 'Authoritative schema mismatch' }
    Push-Location $module
    try {
        go build ./...
        if ($LASTEXITCODE -ne 0) { throw 'Go build failed' }
        go vet ./internal/... ./cmd/daemon ./cmd/backendcheck ./cmd/backendexport ./cmd/enginecheck ./cmd/runtimecheck ./cmd/workspacecheck ./cmd/replaycheck ./cmd/attachmentcheck ./cmd/servicescheck ./cmd/gitwritecheck ./cmd/scheduledcheck ./cmd/skillscheck ./cmd/nativecheck ./cmd/mygo ./cmd/quotacheck ./cmd/reviewcheck ./cmd/listingcheck ./cmd/worktreecheck ./cmd/taskeditcheck ./cmd/cleanupcheck ./cmd/runtimeforkcheck ./cmd/updatecheck ./cmd/managementcheck
        if ($LASTEXITCODE -ne 0) { throw 'Go vet failed' }
        go vet ./cmd/usagecheck ./cmd/capabilitycheck ./cmd/mcpauthcheck ./cmd/activitycheck ./cmd/subagentcheck ./cmd/metadatacheck ./cmd/claudequotacheck ./cmd/goalcheck ./cmd/shellworkcheck ./cmd/navigationcheck ./cmd/workspaceservicecheck ./cmd/gitremotecheck ./cmd/skillattachcheck
        if ($LASTEXITCODE -ne 0) { throw 'Additional daemon checks vet failed' }
        foreach ($name in @('daemon', 'backendcheck', 'backendexport', 'enginecheck', 'runtimecheck', 'workspacecheck', 'replaycheck', 'attachmentcheck', 'servicescheck', 'gitwritecheck', 'scheduledcheck', 'skillscheck', 'nativecheck', 'mygo', 'quotacheck', 'reviewcheck', 'listingcheck', 'worktreecheck', 'taskeditcheck', 'cleanupcheck', 'runtimeforkcheck', 'updatecheck', 'managementcheck')) {
            go build -o (Join-Path $output ($name + '.exe')) ('./cmd/' + $name)
            if ($LASTEXITCODE -ne 0) { throw ('Build failed: ' + $name) }
        }
        foreach ($name in @('usagecheck', 'capabilitycheck', 'mcpauthcheck', 'activitycheck', 'subagentcheck', 'metadatacheck', 'claudequotacheck', 'goalcheck', 'shellworkcheck', 'navigationcheck', 'workspaceservicecheck', 'gitremotecheck', 'skillattachcheck')) {
            go build -o (Join-Path $output ($name + '.exe')) ('./cmd/' + $name)
            if ($LASTEXITCODE -ne 0) { throw ('Build failed: ' + $name) }
        }
    } finally { Pop-Location }
    & (Join-Path $output 'backendcheck.exe') --daemon (Join-Path $output 'daemon.exe') --exporter (Join-Path $output 'backendexport.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Backend storage E2E failed' }
    & (Join-Path $output 'runtimecheck.exe') --daemon (Join-Path $output 'daemon.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Runtime E2E failed' }
    & (Join-Path $output 'taskeditcheck.exe') --daemon (Join-Path $output 'daemon.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Task history edit E2E failed' }
    & (Join-Path $output 'cleanupcheck.exe') --daemon (Join-Path $output 'daemon.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Retention cleanup E2E failed' }
    & (Join-Path $output 'runtimeforkcheck.exe') --daemon (Join-Path $output 'daemon.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Runtime fork E2E failed' }
    & (Join-Path $output 'updatecheck.exe') --daemon (Join-Path $output 'daemon.exe')
    if ($LASTEXITCODE -ne 0) { throw 'OpenCode update check E2E failed' }
    & (Join-Path $output 'managementcheck.exe') --daemon (Join-Path $output 'daemon.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Profile-local OpenCode management E2E failed' }
    foreach ($name in @('usagecheck', 'capabilitycheck', 'mcpauthcheck', 'activitycheck', 'subagentcheck', 'metadatacheck', 'claudequotacheck', 'goalcheck', 'shellworkcheck', 'navigationcheck', 'workspaceservicecheck', 'gitremotecheck', 'skillattachcheck')) {
        & (Join-Path $output ($name + '.exe')) --daemon (Join-Path $output 'daemon.exe')
        if ($LASTEXITCODE -ne 0) { throw ('Additional daemon E2E failed: ' + $name) }
    }
    & (Join-Path $output 'subagentcheck.exe') --daemon (Join-Path $output 'daemon.exe') --metadata-budget-only
    if ($LASTEXITCODE -ne 0) { throw 'Background metadata snapshot budget E2E failed' }
    & (Join-Path $output 'workspacecheck.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Workspace E2E failed' }
    & (Join-Path $output 'replaycheck.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Replay E2E failed' }
    & (Join-Path $output 'gitwritecheck.exe') --daemon (Join-Path $output 'daemon.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Git writes and native status E2E failed' }
    foreach ($name in @('attachmentcheck', 'servicescheck', 'scheduledcheck', 'skillscheck', 'nativecheck', 'quotacheck', 'reviewcheck', 'listingcheck', 'worktreecheck')) {
        & (Join-Path $output ($name + '.exe'))
        if ($LASTEXITCODE -ne 0) { throw ('Additional backend check failed: ' + $name) }
    }
    if ($OpenCodeBinary) {
        & (Join-Path $output 'enginecheck.exe') --binary $OpenCodeBinary
    } else {
        & (Join-Path $output 'enginecheck.exe')
    }
    if ($LASTEXITCODE -ne 0) { throw 'Engine E2E failed' }
    & (Join-Path $output 'mygo.exe') --root $root --profile agent --backend go --verify --daemon (Join-Path $output 'daemon.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Go launcher E2E failed' }
    Write-Output '[OK] Isolated Go backend checks passed; inspect each report for unsupported or skipped scenarios.'
} finally { Pop-Location }
