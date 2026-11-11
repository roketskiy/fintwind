//go:build windows

package main

import (
	"context"
	"fmt"
	"path/filepath"
	"sync"
	"syscall"
	"time"
)

func buildLock(ctx context.Context, directory string) (func(), error) {
	path, err := syscall.UTF16PtrFromString(filepath.Join(directory, "build.lock"))
	if err != nil {
		return nil, err
	}
	for {
		handle, err := syscall.CreateFile(path, syscall.GENERIC_READ|syscall.GENERIC_WRITE, 0, nil, syscall.OPEN_ALWAYS, syscall.FILE_ATTRIBUTE_NORMAL, 0)
		if err == nil {
			var once sync.Once
			return func() { once.Do(func() { _ = syscall.CloseHandle(handle) }) }, nil
		}
		if err != syscall.Errno(32) && err != syscall.Errno(33) {
			return nil, err
		}
		select {
		case <-ctx.Done():
			return nil, fmt.Errorf("another launcher is building this profile: %w", ctx.Err())
		case <-time.After(200 * time.Millisecond):
		}
	}
}
