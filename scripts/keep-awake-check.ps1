# Verifies the Win32 execution-state pair the app uses for "keep screen awake":
# the same flags must make the system report ES_DISPLAY_REQUIRED, and releasing
# them must return the system to its previous state. Runs in this process only;
# the thread exits (and the request dies with it) when the script ends.
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class KeepAwakeProbe
{
    public const uint Continuous = 0x80000000;
    public const uint DisplayRequired = 0x00000002;
    public const uint SystemRequired = 0x00000001;
    private const int SystemExecutionState = 16;

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint SetThreadExecutionState(uint flags);

    [DllImport("powrprof.dll")]
    private static extern uint CallNtPowerInformation(
        int level, IntPtr input, uint inputSize, out uint output, uint outputSize);

    public static uint Apply(uint flags) => SetThreadExecutionState(flags);

    public static uint Read()
    {
        uint state;
        uint status = CallNtPowerInformation(SystemExecutionState, IntPtr.Zero, 0, out state, 4);
        if (status != 0)
        {
            throw new InvalidOperationException(String.Format("CallNtPowerInformation failed: 0x{0:X8}", status));
        }
        return state;
    }
}
'@

$baseline = [KeepAwakeProbe]::Read()
$previous = [KeepAwakeProbe]::Apply([KeepAwakeProbe]::Continuous -bor [KeepAwakeProbe]::DisplayRequired -bor [KeepAwakeProbe]::SystemRequired)
if ($previous -eq 0) {
    Write-Host '[X] SetThreadExecutionState rejected the request'
    exit 1
}
$held = [KeepAwakeProbe]::Read()
$null = [KeepAwakeProbe]::Apply([KeepAwakeProbe]::Continuous)
$released = [KeepAwakeProbe]::Read()

Write-Host ('baseline 0x{0:X8}' -f $baseline)
Write-Host ('held     0x{0:X8}' -f $held)
Write-Host ('released 0x{0:X8}' -f $released)

if (($held -band [KeepAwakeProbe]::DisplayRequired) -eq 0) {
    Write-Host '[X] the display requirement was not observed while held'
    exit 1
}
if (($held -band [KeepAwakeProbe]::SystemRequired) -eq 0) {
    Write-Host '[X] the system requirement was not observed while held'
    exit 1
}
if (($released -band [KeepAwakeProbe]::DisplayRequired) -ne ($baseline -band [KeepAwakeProbe]::DisplayRequired)) {
    Write-Host '[X] releasing the request did not restore the baseline'
    exit 1
}
Write-Host '[OK] the display and system requirements are held and released'
