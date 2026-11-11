using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

// Only handles created in Run are cleaned up. A suspended child joins its
// kill-on-close Job before any user code or descendant can execute.
public static class MyGoJob
{
    [StructLayout(LayoutKind.Sequential)]
    struct Limits { public long ProcessTime, JobTime; public uint Flags; public UIntPtr Min, Max; public uint Active; public UIntPtr Affinity; public uint Priority, Scheduling; }
    [StructLayout(LayoutKind.Sequential)]
    struct IO { public ulong ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes; }
    [StructLayout(LayoutKind.Sequential)]
    struct Extended { public Limits Basic; public IO Io; public UIntPtr ProcessMemory, JobMemory, PeakProcess, PeakJob; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct Startup { public uint Size; public string Reserved, Desktop, Title; public uint X, Y, Width, Height, XChars, YChars, Fill, Flags; public ushort Show, ExtraSize; public IntPtr Extra, Input, Output, Error; }
    [StructLayout(LayoutKind.Sequential)]
    struct ProcessInfo { public IntPtr Process, Thread; public uint ProcessId, ThreadId; }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr CreateJobObjectW(IntPtr security, string name);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetInformationJobObject(IntPtr job, int kind, ref Extended info, uint size);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool CreateProcessW(string app, StringBuilder command, IntPtr ps, IntPtr ts, bool inherit, uint flags, IntPtr environment, string directory, ref Startup startup, out ProcessInfo process);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll", SetLastError = true)] static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll")] static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetExitCodeProcess(IntPtr process, out uint code);
    [DllImport("kernel32.dll")] static extern bool TerminateProcess(IntPtr process, uint code);
    [DllImport("kernel32.dll")] static extern bool TerminateJobObject(IntPtr job, uint code);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);

    static string Quote(string value)
    {
        var text = new StringBuilder("\"");
        int slashes = 0;
        foreach (char c in value)
        {
            if (c == '\\') { slashes++; continue; }
            if (c == '"') { text.Append('\\', slashes * 2 + 1); text.Append(c); slashes = 0; continue; }
            text.Append('\\', slashes); slashes = 0; text.Append(c);
        }
        text.Append('\\', slashes * 2); text.Append('"');
        return text.ToString();
    }

    public static int Run(string executable, string[] args, string directory, int timeoutMs)
    {
        IntPtr job = CreateJobObjectW(IntPtr.Zero, null);
        if (job == IntPtr.Zero) throw new Win32Exception();
        var process = new ProcessInfo();
        bool assigned = false;
        try
        {
            var limits = new Extended(); limits.Basic.Flags = 0x2000;
            if (!SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf<Extended>())) throw new Win32Exception();
            var command = new StringBuilder(Quote(executable));
            foreach (string arg in args) command.Append(' ').Append(Quote(arg));
            var startup = new Startup(); startup.Size = (uint)Marshal.SizeOf<Startup>();
            if (!CreateProcessW(executable, command, IntPtr.Zero, IntPtr.Zero, false, 4, IntPtr.Zero, directory, ref startup, out process)) throw new Win32Exception();
            if (!AssignProcessToJobObject(job, process.Process)) throw new Win32Exception();
            assigned = true;
            if (ResumeThread(process.Thread) == uint.MaxValue) throw new Win32Exception();
            uint wait = WaitForSingleObject(process.Process, timeoutMs < 0 ? uint.MaxValue : (uint)timeoutMs);
            if (wait == 258) throw new TimeoutException("MyGo build exceeded its time limit.");
            if (wait != 0) throw new Win32Exception();
            uint exit;
            if (!GetExitCodeProcess(process.Process, out exit)) throw new Win32Exception();
            return unchecked((int)exit);
        }
        finally
        {
            if (process.Process != IntPtr.Zero)
            {
                if (assigned) TerminateJobObject(job, 1); else TerminateProcess(process.Process, 1);
                WaitForSingleObject(process.Process, 5000);
                CloseHandle(process.Process);
            }
            if (process.Thread != IntPtr.Zero) CloseHandle(process.Thread);
            CloseHandle(job);
        }
    }
}
