param([ValidateSet('stable', 'beta')][string]$Channel = 'stable')
$ErrorActionPreference = 'Stop'
Push-Location $PSScriptRoot
try {
    & bun scripts/bundle-windows.ts "--channel=$Channel"
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
