param([switch]$SkipBuild)

$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path $PSScriptRoot -Parent
if (-not $SkipBuild) {
    Push-Location $taskRoot
    try {
        cargo build --locked -p fintwind-core --example general_workspace_blackbox
        if ($LASTEXITCODE -ne 0) { throw 'Black-box build failed.' }
    } finally { Pop-Location }
}

$taskOutput = Join-Path $taskRoot 'temp/general-workspace-checks'
New-Item -ItemType Directory -Force -Path $taskOutput | Out-Null
$taskProfile = Join-Path $taskOutput ([guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $taskProfile | Out-Null
$taskStart = [System.Diagnostics.ProcessStartInfo]::new()
$taskStart.FileName = Join-Path $taskRoot 'target/debug/examples/general_workspace_blackbox.exe'
$taskStart.UseShellExecute = $false
$taskStart.CreateNoWindow = $true
$taskStart.RedirectStandardOutput = $true
$taskStart.RedirectStandardError = $true
$taskStart.WorkingDirectory = $taskProfile
$taskStart.ArgumentList.Add($taskOutput)
$taskStart.Environment.Clear()
foreach ($taskName in @('SystemRoot', 'WINDIR', 'TEMP', 'TMP')) {
    $taskValue = [Environment]::GetEnvironmentVariable($taskName)
    if ($taskValue) { $taskStart.Environment[$taskName] = $taskValue }
}
$taskStart.Environment['PATH'] = Join-Path $env:SystemRoot 'System32'
$taskStart.Environment['USERPROFILE'] = $taskProfile
$taskStart.Environment['HOME'] = $taskProfile
$taskStart.Environment['APPDATA'] = Join-Path $taskProfile 'AppData/Roaming'
$taskStart.Environment['LOCALAPPDATA'] = Join-Path $taskProfile 'AppData/Local'

# The executable uses only in-process RPC threads, never child processes.
# Retain its handle; timeout cleanup can only terminate this exact process.
$taskProcess = [System.Diagnostics.Process]::Start($taskStart)
try {
    $taskStdout = $taskProcess.StandardOutput.ReadToEndAsync()
    $taskStderr = $taskProcess.StandardError.ReadToEndAsync()
    if (-not $taskProcess.WaitForExit(45000)) {
        $taskProcess.Kill()
        $taskProcess.WaitForExit()
        throw 'Owned black-box process timed out.'
    }
    $taskStdout.GetAwaiter().GetResult()
    $taskStderr.GetAwaiter().GetResult()
    if ($taskProcess.ExitCode -ne 0) { throw "Black-box check failed: $($taskProcess.ExitCode)" }
} finally { $taskProcess.Dispose() }
