param([string]$Binary = '', [string]$Label = 'memory-save')
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (-not $Binary) {
    $Binary = Join-Path (Join-Path (Join-Path (Join-Path $repo 'target') 'debug') 'examples') 'memory_save_blackbox.exe'
}
if (-not (Test-Path -LiteralPath $Binary)) { throw 'Build the fintwind-client memory_save_blackbox example first.' }
$run = Join-Path 'C:/Users/Public/Temp/opencode' ($Label + '-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $run
$isolatedHome = Join-Path $run 'home'
$null = New-Item -ItemType Directory -Path $isolatedHome
$start = [System.Diagnostics.ProcessStartInfo]::new()
$start.FileName = [System.IO.Path]::GetFullPath($Binary)
$start.ArgumentList.Add($run)
$start.WorkingDirectory = $run
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
$start.Environment.Clear()
foreach ($name in @('PATH', 'PATHEXT', 'SystemRoot', 'WINDIR', 'ComSpec', 'SYSTEMDRIVE')) {
    $value = [Environment]::GetEnvironmentVariable($name)
    if ($value) { $start.Environment[$name] = $value }
}
foreach ($name in @('HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'XDG_CONFIG_HOME', 'TEMP', 'TMP')) {
    $start.Environment[$name] = $isolatedHome
}
# This example only connects to its own loopback peer. It never launches a
# provider or child process, so this held process handle is the entire boundary.
$process = [System.Diagnostics.Process]::Start($start)
if (-not $process) { throw 'Could not start the isolated example.' }
$output = $process.StandardOutput.ReadToEndAsync()
$errors = $process.StandardError.ReadToEndAsync()
$samples = [System.Collections.Generic.List[object]]::new()
$clock = [System.Diagnostics.Stopwatch]::StartNew()
try {
    while (-not $process.WaitForExit(100)) {
        $process.Refresh()
        $samples.Add(@{ elapsedMs = $clock.ElapsedMilliseconds; privateBytes = $process.PrivateMemorySize64 })
        if ($clock.Elapsed.TotalSeconds -gt 90) { throw 'Isolated memory-save check exceeded its deadline.' }
    }
    $output.GetAwaiter().GetResult() | Set-Content -LiteralPath (Join-Path $run 'stdout.txt')
    $errors.GetAwaiter().GetResult() | Set-Content -LiteralPath (Join-Path $run 'stderr.txt')
    $samples | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $run 'memory.json')
    if ($process.ExitCode -ne 0) { throw "Memory-save check failed. Artifact: $run" }
    Write-Output "[OK] Artifact: $run"
} finally {
    # Only the process created above is owned; never scan, adopt or stop others.
    if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
    $process.Dispose()
}
