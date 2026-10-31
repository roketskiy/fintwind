param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Test', 'StableRelease')]
    [string]$Mode,
    [Parameter(Mandatory = $true)]
    [string]$LogDirectory
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$arguments = @('test', '--locked')
if ($Mode -eq 'StableRelease') {
    $arguments = @('check', '--locked', '--release', '--package', 'fintwind', '--bin', 'fintwind', '--package', 'fintwind-daemon', '--bin', 'fintwind-daemon')
}
& cargo @arguments 1> (Join-Path $LogDirectory 'cargo.stdout.log') 2> (Join-Path $LogDirectory 'cargo.stderr.log')
exit $LASTEXITCODE
