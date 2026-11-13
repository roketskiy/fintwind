//go:build windows

package main

import (
	"errors"
	"os"
	"path/filepath"

	"golang.org/x/sys/windows"
)

func lockDirectory(directory string) (*os.File, error) {
	path, err := windows.UTF16PtrFromString(filepath.Join(directory, "daemon.lock"))
	if err != nil {
		return nil, err
	}
	handle, err := windows.CreateFile(path, windows.GENERIC_READ|windows.GENERIC_WRITE, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE, nil, windows.OPEN_ALWAYS, windows.FILE_ATTRIBUTE_NORMAL, 0)
	if err != nil {
		return nil, errors.New("cannot open daemon lock")
	}
	if err := windows.LockFileEx(handle, windows.LOCKFILE_EXCLUSIVE_LOCK|windows.LOCKFILE_FAIL_IMMEDIATELY, 0, 1, 0, &windows.Overlapped{}); err != nil {
		windows.CloseHandle(handle)
		return nil, errors.New("data directory already owned")
	}
	return os.NewFile(uintptr(handle), "daemon.lock"), nil
}
