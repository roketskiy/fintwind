//go:build windows

package backendskills

import (
	"errors"
	"runtime"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

// SHFileOperation uses a double-NUL list. Never pass a single terminated
// string: the shell would continue past the buffer and act on unrelated data.
func trashDirectory(path string) error {
	from, err := syscall.UTF16FromString(path)
	if err != nil {
		return err
	}
	from = append(from, 0)
	operation := struct {
		Window   uintptr
		Function uint32
		From     *uint16
		To       *uint16
		Flags    uint16
		Aborted  int32
		Mappings uintptr
		Title    *uint16
	}{Function: 3, From: &from[0], Flags: 0x0040 | 0x0010 | 0x0004 | 0x0400 | 0x0200 | 0x2000}
	proc := windows.NewLazySystemDLL("shell32.dll").NewProc("SHFileOperationW")
	result, _, _ := proc.Call(uintptr(unsafe.Pointer(&operation)))
	runtime.KeepAlive(from)
	if result != 0 || operation.Aborted != 0 {
		return errors.New("could not move skill to Recycle Bin")
	}
	return nil
}
