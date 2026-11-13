param(
    [Parameter(Mandatory = $true)][string]$Harness,
    [Parameter(Mandatory = $true)][string]$Daemon,
    [switch]$MetadataBudgetOnly
)

$ErrorActionPreference = 'Stop'
foreach ($name in @('DEEPSEEK_API_KEY', 'OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'GEMINI_API_KEY', 'FINTWIND_AGENT_REAL_PROVIDER')) {
    Remove-Item -Path ('Env:' + $name) -ErrorAction SilentlyContinue
}

$arguments = @('-daemon', $Daemon)
if ($MetadataBudgetOnly) { $arguments += '-metadata-budget-only' }
& $Harness @arguments
exit $LASTEXITCODE
