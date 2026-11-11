//go:build windows

package main

import (
	"context"
	"errors"
	"runtime"
	"syscall"
)

func runPreferenceAwake(ctx context.Context, report func(error)) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	kernel := syscall.NewLazyDLL("kernel32.dll")
	setState := kernel.NewProc("SetThreadExecutionState")
	result, _, _ := setState.Call(0x80000003)
	if result == 0 {
		report(errors.New("Windows rejected the wake request"))
		return
	}
	report(nil)
	<-ctx.Done()
	setState.Call(0x80000000)
}
