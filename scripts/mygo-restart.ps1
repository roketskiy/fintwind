# Restarts the daily MyGo instance: stops the running launcher precisely
# (mygo.exe --profile daily rooted here; its supervisor's kill-on-close Job
# tears the app and daemon down), then starts MyGo.cmd detached. The launcher
# rebuilds the app from source on every launch, so a restart picks up the
# latest code.
param([switch]$ProbeProvider, [string]$ProbeSession = '')
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$found = @()
foreach ($p in Get-CimInstance Win32_Process -Filter "Name = 'mygo.exe'") {
    if ($p -and $p.CommandLine -and $p.CommandLine -match '(?:^|\s)"?--profile"?\s+"?daily"?(?:\s|$)' -and $p.CommandLine -like "*$root*") {
        $found += $p
    }
}
if ($found.Count -gt 0) {
    foreach ($p in $found) {
        Stop-Process -Id $p.ProcessId -Force
    }
    # The supervisor (pwsh holding the Job) tears the app and daemon down as
    # the launcher dies; give it a moment before starting fresh.
    Start-Sleep -Seconds 3
    Write-Host '[STOP] stopped' $found.Count 'running MyGo instance(s)'
} else {
    Write-Host '[STOP] no running MyGo instance'
}
# Do not claim directly launched app/daemon processes by name or path.
# A conflicting data lock must make the new launcher fail, not widen cleanup.

$launchArgs = @()
if ($ProbeProvider) { $launchArgs += '--probe-provider' }
if ($ProbeSession) { $launchArgs += @('--probe-session', $ProbeSession) }
if ($launchArgs.Count -gt 0) {
    Start-Process -FilePath (Join-Path $root 'MyGo.cmd') -ArgumentList $launchArgs -WorkingDirectory $root -WindowStyle Minimized
} else {
    Start-Process -FilePath (Join-Path $root 'MyGo.cmd') -WorkingDirectory $root -WindowStyle Minimized
}
Write-Host '[START] MyGo.cmd launched'
