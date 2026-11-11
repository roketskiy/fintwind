# Restarts the daily MyGo instance: stops the running launcher precisely
# (mygo.exe --profile daily rooted here; its supervisor's kill-on-close Job
# tears the app and daemon down), then starts MyGo.cmd detached. The launcher
# rebuilds the app from source on every launch, so a restart picks up the
# latest code.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$found = @()
foreach ($p in Get-CimInstance Win32_Process -Filter "Name = 'mygo.exe'") {
    if ($p.CommandLine -match '--profile daily' -and $p.CommandLine -like "*$root*") {
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

Start-Process -FilePath (Join-Path $root 'MyGo.cmd') -WorkingDirectory $root -WindowStyle Minimized
Write-Host '[START] MyGo.cmd launched'
