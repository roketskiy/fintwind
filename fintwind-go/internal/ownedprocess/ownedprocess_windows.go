//go:build windows

package ownedprocess

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"unsafe"
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

// jobExtendedLimits mirrors the repo's proven Job shape: class 9 with the
// full extended struct. A bare class 2 call was rejected on this machine,
// so Start follows scripts/goal-opencode-e2e.py instead of guessing.
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
// stdio lands in roundDir files. extra carries explicit keys like the daemon
// token. It never kills by PID and never touches other processes.
func Start(executable string, args []string, roundDir string, extra map[string]string) (*Owned, error) {
	return StartWithEnvironment(executable, args, roundDir, roundDir, BuildEnv(roundDir, extra))
}

// StartWithEnvironment contains a caller-selected profile in the same owned
// Job as isolated verification. The complete environment must be explicit.
func StartWithEnvironment(executable string, args []string, runDir, workingDir string, environment []string) (*Owned, error) {
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

	stdoutPath := filepath.Join(roundDir, "child-stdout.log")
	stderrPath := filepath.Join(roundDir, "child-stderr.log")
	stdoutFile, err := inheritFile(stdoutPath)
	if err != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, err
	}
	defer stdoutFile.Close()
	stderrFile, err := inheritFile(stderrPath)
	if err != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, err
	}
	defer stderrFile.Close()

	cmdLine, err := syscall.UTF16PtrFromString(commandLine(absolute, args))
	if err != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: command line: %w", err)
	}
	appName, err := syscall.UTF16PtrFromString(absolute)
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

	startup := startupInfo{flags: startfUseStdHandles}
	startup.cb = uint32(unsafe.Sizeof(startup))
	startup.stdInput = syscall.Handle(0)
	startup.stdOutput = syscall.Handle(stdoutFile.Fd())
	startup.stdError = syscall.Handle(stderrFile.Fd())
	var info processInfo
	ok, _, errno = procCreateProcessW.Call(
		uintptr(unsafe.Pointer(appName)),
		uintptr(unsafe.Pointer(cmdLine)),
		0, 0, 1,
		uintptr(createSuspended|createNoWindow|createUnicodeEnvironment),
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
