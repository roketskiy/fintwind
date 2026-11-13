$ErrorActionPreference = 'Stop'
$keys = @('DEEPSEEK_API_KEY', 'OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'FINTWIND_AGENT_REAL_PROVIDER')
foreach ($key in $keys) {
    Remove-Item -Path (Join-Path 'Env:' $key) -ErrorAction SilentlyContinue
}
$runner = 'C:\Users\Public\Temp\opencode\gitremotecheck.exe'
$daemon = 'C:\Users\Public\Temp\opencode\fintwind-daemon-gitremotecheck.exe'
if (-not (Test-Path $runner)) { throw 'E2E runner is missing' }
if (-not (Test-Path $daemon)) { throw 'Daemon executable is missing' }
& $runner --daemon $daemon
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
