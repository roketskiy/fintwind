param(
    [Parameter(Mandatory = $true)][string]$OutputDir,
    [switch]$HarnessOnly
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
Push-Location $root
try {
    if (-not $HarnessOnly) {
        & go build -o (Join-Path $OutputDir 'fintwind-daemon.exe') ./cmd/daemon
        if ($LASTEXITCODE -ne 0) { throw 'daemon build failed' }
    }
    & go build -o (Join-Path $OutputDir 'subagentcheck.exe') ./tools/e2e/subagentcheck
    if ($LASTEXITCODE -ne 0) { throw 'subagentcheck build failed' }
} finally {
    Pop-Location
}
