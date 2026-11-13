$ErrorActionPreference = 'Stop'
$moduleRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
$outputDir = Join-Path $moduleRoot 'target/go-backend-skillattach'
$daemonPath = Join-Path $outputDir 'daemon.exe'
$goalcheckPath = Join-Path $outputDir 'skillattachcheck.exe'

if (-not (Test-Path (Join-Path $moduleRoot 'go.mod'))) {
    throw 'fintwind-go module root was not found'
}

New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
$env:GOPROXY = 'off'
foreach ($name in @(
    'DEEPSEEK_API_KEY', 'OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'GEMINI_API_KEY', 'OPENROUTER_API_KEY', 'GOOGLE_API_KEY', 'AZURE_OPENAI_API_KEY',
    'FINTWIND_AGENT_REAL_PROVIDER', 'OPENCODE_SERVER_PASSWORD', 'OPENCODE_TEST_HOME',
    'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY'
)) {
    Set-Item -Path (Join-Path 'Env:' $name) -Value ''
}

Push-Location $moduleRoot
try {
    $watch = [System.Diagnostics.Stopwatch]::StartNew()

    & go build -o $daemonPath ./cmd/daemon
    if ($LASTEXITCODE -ne 0) { throw "daemon build failed with exit code $LASTEXITCODE" }
    if ($watch.Elapsed.TotalSeconds -gt 240) { throw 'daemon build exceeded 240 seconds' }

    & go build -o $goalcheckPath ./tools/e2e/skillattachcheck
    if ($LASTEXITCODE -ne 0) { throw "skillattachcheck build failed with exit code $LASTEXITCODE" }
    if ($watch.Elapsed.TotalSeconds -gt 360) { throw 'skillattachcheck build exceeded 360 seconds' }

    & go vet ./internal/backendruntime ./internal/backendengine ./internal/ownedprocess ./cmd/daemon ./tools/e2e/skillattachcheck
    if ($LASTEXITCODE -ne 0) { throw "targeted go vet failed with exit code $LASTEXITCODE" }
    if ($watch.Elapsed.TotalSeconds -gt 420) { throw 'targeted go vet exceeded 420 seconds' }

    & $goalcheckPath --daemon $daemonPath
    if ($LASTEXITCODE -ne 0) { throw "skillattachcheck E2E failed with exit code $LASTEXITCODE" }
    if ($watch.Elapsed.TotalSeconds -gt 590) { throw 'build, vet, and E2E exceeded the 590 second safety budget' }
}
finally {
    Pop-Location
}
