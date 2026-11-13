param([string]$OpenCodeBinary = '')
$ErrorActionPreference = 'Stop'
# This subprocess is a credential-free regression runner. Opt-in live-model
# checks use separate scripts; never let their switches reach this suite.
Remove-Item Env:FINTWIND_AGENT_REAL_PROVIDER -ErrorAction SilentlyContinue
Remove-Item Env:DEEPSEEK_API_KEY -ErrorAction SilentlyContinue
Remove-Item Env:OPENAI_API_KEY -ErrorAction SilentlyContinue
Remove-Item Env:ANTHROPIC_API_KEY -ErrorAction SilentlyContinue
$root = Split-Path -Parent $PSScriptRoot
$module = $root
$output = Join-Path $root 'target/go-backend'
New-Item -ItemType Directory -Path $output -Force | Out-Null
$env:HTTP_PROXY = 'http://127.0.0.1:7897'
$env:HTTPS_PROXY = 'http://127.0.0.1:7897'
Push-Location $root
try {
    python scripts/check-repository-layout.py
    if ($LASTEXITCODE -ne 0) { throw 'Repository source layout mismatch' }
    python scripts/generate-go-backend-schema.py --check
    if ($LASTEXITCODE -ne 0) { throw 'Authoritative schema mismatch' }
    Push-Location $module
    try {
        go build ./...
        if ($LASTEXITCODE -ne 0) { throw 'Go build failed' }
        go vet ./internal/... ./cmd/daemon ./tools/e2e/backendcheck ./tools/export ./tools/e2e/enginecheck ./tools/e2e/runtimecheck ./tools/e2e/workspacecheck ./tools/e2e/replaycheck ./tools/e2e/attachmentcheck ./tools/e2e/servicescheck ./tools/e2e/gitwritecheck ./tools/e2e/scheduledcheck ./tools/e2e/skillscheck ./tools/e2e/nativecheck ./cmd/mygo ./tools/e2e/quotacheck ./tools/e2e/reviewcheck ./tools/e2e/listingcheck ./tools/e2e/worktreecheck ./tools/e2e/taskeditcheck ./tools/e2e/cleanupcheck ./tools/e2e/runtimeforkcheck ./tools/e2e/updatecheck ./tools/e2e/managementcheck ./tools/e2e/compositioncheck
        if ($LASTEXITCODE -ne 0) { throw 'Go vet failed' }
        go vet ./tools/e2e/usagecheck ./tools/e2e/capabilitycheck ./tools/e2e/mcpauthcheck ./tools/e2e/activitycheck ./tools/e2e/subagentcheck ./tools/e2e/metadatacheck ./tools/e2e/claudequotacheck ./tools/e2e/goalcheck ./tools/e2e/shellworkcheck ./tools/e2e/navigationcheck ./tools/e2e/workspaceservicecheck ./tools/e2e/gitremotecheck ./tools/e2e/skillattachcheck
        if ($LASTEXITCODE -ne 0) { throw 'Additional daemon checks vet failed' }
        foreach ($name in @('daemon', 'backendcheck', 'backendexport', 'enginecheck', 'runtimecheck', 'workspacecheck', 'replaycheck', 'attachmentcheck', 'servicescheck', 'gitwritecheck', 'scheduledcheck', 'skillscheck', 'nativecheck', 'mygo', 'quotacheck', 'reviewcheck', 'listingcheck', 'worktreecheck', 'taskeditcheck', 'cleanupcheck', 'runtimeforkcheck', 'updatecheck', 'managementcheck', 'compositioncheck')) {
            $source = switch ($name) {
                'daemon' { './cmd/daemon' }
                'mygo' { './cmd/mygo' }
                'backendexport' { './tools/export' }
                default { './tools/e2e/' + $name }
            }
            go build -o (Join-Path $output ($name + '.exe')) $source
            if ($LASTEXITCODE -ne 0) { throw ('Build failed: ' + $name) }
        }
        foreach ($name in @('usagecheck', 'capabilitycheck', 'mcpauthcheck', 'activitycheck', 'subagentcheck', 'metadatacheck', 'claudequotacheck', 'goalcheck', 'shellworkcheck', 'navigationcheck', 'workspaceservicecheck', 'gitremotecheck', 'skillattachcheck')) {
            $source = switch ($name) {
                'daemon' { './cmd/daemon' }
                'mygo' { './cmd/mygo' }
                'backendexport' { './tools/export' }
                default { './tools/e2e/' + $name }
            }
            go build -o (Join-Path $output ($name + '.exe')) $source
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
    if ($OpenCodeBinary) {
        & (Join-Path $output 'compositioncheck.exe') --opencode $OpenCodeBinary
    } else {
        & (Join-Path $output 'compositioncheck.exe')
    }
    if ($LASTEXITCODE -ne 0) { throw 'Context composition E2E failed' }
    & (Join-Path $output 'mygo.exe') --root $root --profile agent --backend go --verify --daemon (Join-Path $output 'daemon.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Go launcher E2E failed' }
    Write-Output '[OK] Isolated Go backend checks passed; inspect each report for unsupported or skipped scenarios.'
} finally { Pop-Location }
