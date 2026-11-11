//go:build !windows

package main

import (
	"syscall"
)

// parentAlive reports whether the parent process still runs. It only probes
// the process handle and never terminates anything.
func parentAlive(pid int) bool {
	return syscall.Kill(pid, 0) == nil
}
