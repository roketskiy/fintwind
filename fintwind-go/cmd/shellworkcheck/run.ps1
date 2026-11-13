$ErrorActionPreference = 'Stop'
$module = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$runner = Join-Path $module 'target\go-backend-shellwork\shellworkcheck.exe'
$daemon = Join-Path $module 'target\go-backend-shellwork\fintwind-daemon.exe'
if (-not (Test-Path $runner) -or -not (Test-Path $daemon)) {
    throw 'Build runner and daemon into target\go-backend-shellwork before running this check.'
}

$keys = @('DEEPSEEK_API_KEY', 'OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'GEMINI_API_KEY', 'FINTWIND_AGENT_REAL_PROVIDER')
$saved = @{}
foreach ($key in $keys) {
    $entry = Get-Item "Env:$key" -ErrorAction SilentlyContinue
    if ($entry) {
        $saved[$key] = $entry.Value
        Remove-Item "Env:$key"
    }
}

$exitCode = 1
try {
    & $runner --daemon $daemon
    $exitCode = $LASTEXITCODE
}
finally {
    foreach ($key in $saved.Keys) {
        Set-Item "Env:$key" $saved[$key]
    }
}
exit $exitCode
