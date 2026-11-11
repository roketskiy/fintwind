//go:build windows

package main

import (
	"syscall"
	"unsafe"
)

var (
	kernel32        = syscall.NewLazyDLL("kernel32.dll")
	procOpenProcess = kernel32.NewProc("OpenProcess")
	procGetExitCode = kernel32.NewProc("GetExitCodeProcess")
	procCloseHandle = kernel32.NewProc("CloseHandle")
)

const (
	processQueryLimited = 0x1000
	stillActive         = 259
)

// parentAlive reports whether the parent process still runs. It only reads
// the process handle and never terminates anything.
func parentAlive(pid int) bool {
	handle, _, _ := procOpenProcess.Call(processQueryLimited, 0, uintptr(pid))
	if handle == 0 {
		return false
	}
	defer procCloseHandle.Call(handle)
	var code uint32
	ok, _, _ := procGetExitCode.Call(handle, uintptr(unsafe.Pointer(&code)))
	if ok == 0 {
		return false
	}
	return code == stillActive
}
