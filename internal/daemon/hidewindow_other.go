//go:build !windows

package daemon

import "os/exec"

func hideWindow(cmd *exec.Cmd) {}
