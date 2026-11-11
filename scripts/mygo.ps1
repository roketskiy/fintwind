param(
    [ValidateSet('agent', 'daily')][string]$Profile = 'agent',
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$LauncherArgs
)
$ErrorActionPreference = 'Stop'

try {
    foreach ($argument in $LauncherArgs) {
        if ($argument -match '^--?(profile|root)(=|$)') {
            throw 'Use the matching MyGo entry point; profile and root cannot be overridden.'
        }
    }
    $root = Split-Path -Parent $PSScriptRoot
    $go = Get-Command 'go.exe' -CommandType Application -ErrorAction Stop
    $module = Join-Path $root 'fintwind-go'
    $bootstrap = Join-Path $root ('target/mygo/bootstrap/' + $Profile)
    $null = New-Item -ItemType Directory -Path $bootstrap -Force
    $round = Join-Path $bootstrap ([Guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path $round
    $launcher = Join-Path $round 'mygo.exe'
    Add-Type -Path (Join-Path $PSScriptRoot 'mygo-job.cs')
    Write-Host '[BUILD] MyGo launcher'
    $buildArgs = @('build', '-mod=readonly', '-o', $launcher, './cmd/mygo')
    $code = [MyGoJob]::Run($go.Source, $buildArgs, $module, 600000)
    if ($code -ne 0) { throw ('Launcher build failed (exit ' + $code + '). No old binary was launched.') }
    $appArgs = @('--root', $root, '--profile', $Profile) + $LauncherArgs
    # Build is bounded above. The app is interactive and runs until closed.
    $code = [MyGoJob]::Run($launcher, $appArgs, $root, -1)
    exit $code
} catch {
    Write-Host ('[X] ' + $_.Exception.Message)
    exit 1
}
