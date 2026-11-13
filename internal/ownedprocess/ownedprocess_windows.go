//go:build windows

package ownedprocess

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	kernel32                     = syscall.NewLazyDLL("kernel32.dll")
	procCreateJobObjectW         = kernel32.NewProc("CreateJobObjectW")
	procSetInformationJobObject  = kernel32.NewProc("SetInformationJobObject")
	procAssignProcessToJobObject = kernel32.NewProc("AssignProcessToJobObject")
	procCreateProcessW           = kernel32.NewProc("CreateProcessW")
	procResumeThread             = kernel32.NewProc("ResumeThread")
	procTerminateJobObject       = kernel32.NewProc("TerminateJobObject")
	procCloseHandle              = kernel32.NewProc("CloseHandle")
	procSetHandleInformation     = kernel32.NewProc("SetHandleInformation")
	procWaitForSingleObject      = kernel32.NewProc("WaitForSingleObject")
	procGetExitCodeProcess       = kernel32.NewProc("GetExitCodeProcess")
	procTerminateProcess         = kernel32.NewProc("TerminateProcess")
)

const (
	createSuspended              = 0x00000004
	createNoWindow               = 0x08000000
	createUnicodeEnvironment     = 0x00000400
	startfUseStdHandles          = 0x00000100
	handleFlagInherit            = 0x00000001
	jobObjectExtendedLimit       = 9
	jobObjectLimitKillOnJobClose = 0x00002000
	waitObject0                  = 0
	waitTimeout                  = 0x00000102
)

type jobBasicLimits struct {
	perProcessUserTimeLimit int64
	perJobUserTimeLimit     int64
	limitFlags              uint32
	minimumWorkingSetSize   uintptr
	maximumWorkingSetSize   uintptr
	activeProcessLimit      uint32
	affinityMask            uintptr
	priorityClass           uint32
	schedulingClass         uint32
}

// jobExtendedLimits is the Job shape Start uses: class 9 with the full
// extended struct. A bare class 2 call was rejected on this machine, so
// Start supplies the full class 9 structure before resuming the child.
type jobExtendedLimits struct {
	basic                jobBasicLimits
	ioInfo               [6]uint64
	processMemoryLimit   uintptr
	jobMemoryLimit       uintptr
	peakProcessMemoryUse uintptr
	peakJobMemoryUse     uintptr
}

type startupInfo struct {
	cb            uint32
	reserved      *uint16
	desktop       *uint16
	title         *uint16
	x             uint32
	y             uint32
	xSize         uint32
	ySize         uint32
	xCountChars   uint32
	yCountChars   uint32
	fillAttribute uint32
	flags         uint32
	showWindow    uint16
	cbReserved2   uint16
	reserved2     *byte
	stdInput      syscall.Handle
	stdOutput     syscall.Handle
	stdError      syscall.Handle
}

type processInfo struct {
	process   syscall.Handle
	thread    syscall.Handle
	processID uint32
	threadID  uint32
}

// Start launches executable inside a kill-on-close Job and returns before the
// child can run outside it: CREATE_SUSPENDED, assign to Job, then resume.
// stdio lands in roundDir files. extra carries explicit keys such as the
// daemon token. It never kills by PID and never touches other processes.
func Start(executable string, args []string, roundDir string, extra map[string]string) (*Owned, error) {
	return StartWithEnvironment(executable, args, roundDir, roundDir, BuildEnv(roundDir, extra))
}

// StartWithEnvironment contains a caller-selected profile in the same owned
// Job as isolated verification. The complete environment must be explicit.
func StartWithEnvironment(executable string, args []string, runDir, workingDir string, environment []string) (*Owned, error) {
	return startWithEnvironmentInput(executable, args, runDir, workingDir, environment, nil)
}

func startWithEnvironmentInput(executable string, args []string, runDir, workingDir string, environment []string, input *os.File) (*Owned, error) {
	return startWithStreams(executable, args, runDir, workingDir, environment, input, nil, nil)
}

// StartWithOutput directs output to caller-owned pipes without log files.
// Only duplicated write handles are inherited; caller handles are unchanged.
func StartWithOutput(executable string, args []string, runDir, workingDir string, environment []string, stdout, stderr *os.File) (*Owned, error) {
	if stdout == nil || stderr == nil {
		return nil, fmt.Errorf("ownedprocess: both output streams are required")
	}
	return startWithStreams(executable, args, runDir, workingDir, environment, nil, stdout, stderr)
}

func startWithStreams(executable string, args []string, runDir, workingDir string, environment []string, input, stdoutFile, stderrFile *os.File) (*Owned, error) {
	if len(environment) == 0 {
		return nil, fmt.Errorf("ownedprocess: an explicit nonempty environment is required")
	}
	for _, entry := range environment {
		if _, err := syscall.UTF16FromString(entry); err != nil {
			return nil, fmt.Errorf("ownedprocess: invalid environment entry")
		}
	}
	roundDir := runDir
	absolute, err := filepath.Abs(executable)
	if err != nil {
		return nil, fmt.Errorf("ownedprocess: resolve executable: %w", err)
	}
	if _, err := os.Stat(absolute); err != nil {
		return nil, fmt.Errorf("ownedprocess: executable not found: %w", err)
	}
	if roundDir == "" {
		return nil, fmt.Errorf("ownedprocess: round dir is required")
	}

	job, _, _ := procCreateJobObjectW.Call(0, 0)
	if job == 0 {
		return nil, fmt.Errorf("ownedprocess: CreateJobObjectW failed")
	}
	limits := jobExtendedLimits{}
	limits.basic.limitFlags = jobObjectLimitKillOnJobClose
	ok, _, errno := procSetInformationJobObject.Call(job,
		uintptr(jobObjectExtendedLimit),
		uintptr(unsafe.Pointer(&limits)),
		uintptr(unsafe.Sizeof(limits)))
	if ok == 0 {
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: SetInformationJobObject failed: %v (size=%d)", errno, unsafe.Sizeof(limits))
	}

	stdoutPath, stderrPath := "", ""
	if stdoutFile == nil {
		stdoutPath = filepath.Join(roundDir, "child-stdout.log")
		stdoutFile, err = inheritFile(stdoutPath)
		if err != nil {
			_, _, _ = procCloseHandle.Call(job)
			return nil, err
		}
		defer stdoutFile.Close()
	}
	if stderrFile == nil {
		stderrPath = filepath.Join(roundDir, "child-stderr.log")
		stderrFile, err = inheritFile(stderrPath)
		if err != nil {
			_, _, _ = procCloseHandle.Call(job)
			return nil, err
		}
		defer stderrFile.Close()
	}

	application, line, lineErr := ptyCommand(absolute, args)
	if lineErr != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, lineErr
	}
	cmdLine, err := syscall.UTF16PtrFromString(line)
	if err != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: command line: %w", err)
	}
	appName, err := syscall.UTF16PtrFromString(application)
	if err != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: executable: %w", err)
	}
	curDir, err := syscall.UTF16PtrFromString(workingDir)
	if err != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: round dir: %w", err)
	}
	envBlock := envBlock(environment)
	var envPtr unsafe.Pointer
	if envBlock != nil {
		envPtr = unsafe.Pointer(&envBlock[0])
	}

	// A precise handle list prevents a parallel child from retaining another
	// flow's pipe write end and keeping its readers alive indefinitely.
	var inherited []windows.Handle
	defer func() {
		for _, handle := range inherited {
			_ = windows.CloseHandle(handle)
		}
	}()
	duplicate := func(file *os.File) (windows.Handle, error) {
		if file == nil {
			return 0, nil
		}
		var handle windows.Handle
		err := windows.DuplicateHandle(windows.CurrentProcess(), windows.Handle(file.Fd()), windows.CurrentProcess(), &handle, 0, true, windows.DUPLICATE_SAME_ACCESS)
		if err == nil {
			inherited = append(inherited, handle)
		}
		return handle, err
	}
	startup := windows.StartupInfoEx{}
	startup.Cb = uint32(unsafe.Sizeof(startup))
	startup.Flags = startfUseStdHandles
	startup.StdInput, err = duplicate(input)
	if err == nil {
		startup.StdOutput, err = duplicate(stdoutFile)
	}
	if err == nil {
		startup.StdErr, err = duplicate(stderrFile)
	}
	if err != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: duplicate stdio handle: %w", err)
	}
	attributes, err := windows.NewProcThreadAttributeList(1)
	if err != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, err
	}
	defer attributes.Delete()
	if err := attributes.Update(windows.PROC_THREAD_ATTRIBUTE_HANDLE_LIST, unsafe.Pointer(&inherited[0]), uintptr(len(inherited))*unsafe.Sizeof(inherited[0])); err != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, err
	}
	startup.ProcThreadAttributeList = attributes.List()
	var info processInfo
	ok, _, errno = procCreateProcessW.Call(
		uintptr(unsafe.Pointer(appName)),
		uintptr(unsafe.Pointer(cmdLine)),
		0, 0, 1,
		uintptr(createSuspended|createNoWindow|createUnicodeEnvironment|windows.EXTENDED_STARTUPINFO_PRESENT),
		uintptr(envPtr),
		uintptr(unsafe.Pointer(curDir)),
		uintptr(unsafe.Pointer(&startup)),
		uintptr(unsafe.Pointer(&info)))
	if ok == 0 {
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: CreateProcessW: %v", errno)
	}
	ok, _, _ = procAssignProcessToJobObject.Call(job, uintptr(info.process))
	if ok == 0 {
		_, _, _ = procTerminateProcess.Call(uintptr(info.process), 1)
		_, _, _ = procWaitForSingleObject.Call(uintptr(info.process), 5000)
		_, _, _ = procCloseHandle.Call(uintptr(info.thread))
		_, _, _ = procCloseHandle.Call(uintptr(info.process))
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: AssignProcessToJobObject failed")
	}
	resumeResult, _, resumeErr := procResumeThread.Call(uintptr(info.thread))
	_, _, _ = procCloseHandle.Call(uintptr(info.thread))
	if uint32(resumeResult) == 0xffffffff {
		_, _, _ = procTerminateJobObject.Call(job, 1)
		_, _, _ = procWaitForSingleObject.Call(uintptr(info.process), 5000)
		_, _, _ = procCloseHandle.Call(uintptr(info.process))
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: ResumeThread failed: %v", resumeErr)
	}

	jobHandle := syscall.Handle(job)
	procHandle := syscall.Handle(info.process)
	owned := &Owned{
		PID:        info.processID,
		Dir:        roundDir,
		StdoutPath: stdoutPath,
		StderrPath: stderrPath,
	}
	owned.close = func() error {
		_, _, _ = procTerminateJobObject.Call(uintptr(jobHandle), 1)
		status, _, _ := procWaitForSingleObject.Call(uintptr(procHandle), 5000)
		_ = syscall.CloseHandle(procHandle)
		_ = syscall.CloseHandle(jobHandle)
		if uint32(status) != waitObject0 {
			return fmt.Errorf("ownedprocess: child did not exit before cleanup deadline: %d", uint32(status))
		}
		return nil
	}
	owned.exited = func(timeoutMs uint32) (bool, error) {
		status, _, _ := procWaitForSingleObject.Call(uintptr(procHandle), uintptr(timeoutMs))
		switch uint32(status) {
		case waitObject0:
			return true, nil
		case waitTimeout:
			return false, nil
		default:
			return false, fmt.Errorf("ownedprocess: wait failed: %d", uint32(status))
		}
	}
	owned.exitCode = func() (uint32, error) {
		var code uint32
		if err := syscall.GetExitCodeProcess(procHandle, &code); err != nil {
			return 0, err
		}
		return code, nil
	}
	return owned, nil
}

func inheritFile(path string) (*os.File, error) {
	file, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, fmt.Errorf("ownedprocess: open %s: %w", path, err)
	}
	handle := uintptr(syscall.Handle(file.Fd()))
	ok, _, _ := procSetHandleInformation.Call(handle, handleFlagInherit, handleFlagInherit)
	if ok == 0 {
		_ = file.Close()
		return nil, fmt.Errorf("ownedprocess: inherit handle for %s", path)
	}
	return file, nil
}

func commandLine(executable string, args []string) string {
	parts := make([]string, 0, len(args)+1)
	parts = append(parts, syscall.EscapeArg(executable))
	for _, arg := range args {
		parts = append(parts, syscall.EscapeArg(arg))
	}
	return strings.Join(parts, " ")
}

func envBlock(env []string) []uint16 {
	if len(env) == 0 {
		return nil
	}
	var block []uint16
	for _, kv := range env {
		part, err := syscall.UTF16FromString(kv)
		if err != nil {
			continue
		}
		block = append(block, part...)
	}
	block = append(block, 0)
	return block
}
