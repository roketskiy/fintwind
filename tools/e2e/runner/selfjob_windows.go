//go:build windows

package main

import (
	"fmt"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	jobKernel32              = syscall.NewLazyDLL("kernel32.dll")
	createJobObjectW         = jobKernel32.NewProc("CreateJobObjectW")
	setInformationJobObject  = jobKernel32.NewProc("SetInformationJobObject")
	assignProcessToJobObject = jobKernel32.NewProc("AssignProcessToJobObject")
	helperSelfJob            windows.Handle
)

const (
	helperJobObjectExtendedLimit = 9
	helperJobLimitKillOnClose    = 0x00002000
)

type helperJobBasicLimits struct {
	PerProcessUserTimeLimit int64
	PerJobUserTimeLimit     int64
	LimitFlags              uint32
	MinimumWorkingSetSize   uintptr
	MaximumWorkingSetSize   uintptr
	ActiveProcessLimit      uint32
	AffinityMask            uintptr
	PriorityClass           uint32
	SchedulingClass         uint32
}

type helperJobExtendedLimits struct {
	Basic                helperJobBasicLimits
	IOInfo               [6]uint64
	ProcessMemoryLimit   uintptr
	JobMemoryLimit       uintptr
	PeakProcessMemoryUse uintptr
	PeakJobMemoryUse     uintptr
}

// containCurrentProcess assigns this helper to its own kill-on-close Job.
// The helper starts no child before this succeeds. The wrapper may terminate
// only its directly held helper Process handle; closing this process then
// closes this Job and ends any contained helper-owned command work.
func containCurrentProcess() error {
	job, _, createErr := createJobObjectW.Call(0, 0)
	if job == 0 {
		return fmt.Errorf("CreateJobObjectW failed: %v", createErr)
	}
	limits := helperJobExtendedLimits{}
	limits.Basic.LimitFlags = helperJobLimitKillOnClose
	ok, _, setErr := setInformationJobObject.Call(job,
		uintptr(helperJobObjectExtendedLimit),
		uintptr(unsafe.Pointer(&limits)),
		uintptr(unsafe.Sizeof(limits)))
	if ok == 0 {
		_, _, _ = jobKernel32.NewProc("CloseHandle").Call(job)
		return fmt.Errorf("SetInformationJobObject failed: %v", setErr)
	}
	ok, _, assignErr := assignProcessToJobObject.Call(job, uintptr(windows.CurrentProcess()))
	if ok == 0 {
		_, _, _ = jobKernel32.NewProc("CloseHandle").Call(job)
		return fmt.Errorf("AssignProcessToJobObject(self) failed: %v", assignErr)
	}
	helperSelfJob = windows.Handle(job)
	return nil
}
