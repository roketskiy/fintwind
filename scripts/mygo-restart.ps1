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
# Direct-launched daily windows (run-*/fintwind-mygo.exe, no launcher Job)
# are invisible to the match above; replace them too so the fresh window is
# the only one. Scoped to this repo's daily dir: agent profiles, other
# checkouts and installed Fintwind are untouched.
$daily = Join-Path $root 'target/mygo/daily'
$stray = @()
foreach ($p in Get-Process -ErrorAction SilentlyContinue) {
    if ($p.ProcessName -ne 'fintwind-mygo' -and $p.ProcessName -ne 'fintwind-daemon') {
        continue
    }
    try { $path = $p.Path } catch { continue }
    if ([string]::IsNullOrEmpty($path)) {
        continue
    }
    if ($path.StartsWith($daily, [System.StringComparison]::OrdinalIgnoreCase)) {
        $stray += $p
    }
}
if ($stray.Count -gt 0) {
    foreach ($p in $stray) {
        Stop-Process -Id $p.Id -Force
    }
    Start-Sleep -Seconds 2
    Write-Host '[STOP] replaced' $stray.Count 'direct-launched daily window(s)'
}

Start-Process -FilePath (Join-Path $root 'MyGo.cmd') -WorkingDirectory $root -WindowStyle Minimized
Write-Host '[START] MyGo.cmd launched'
