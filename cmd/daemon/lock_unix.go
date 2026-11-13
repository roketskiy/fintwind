//go:build unix

package main

import (
	"os"
	"path/filepath"

	"golang.org/x/sys/unix"
)

func lockDirectory(directory string) (*os.File, error) {
	file, err := os.OpenFile(filepath.Join(directory, "daemon.lock"), os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, err
	}
	if err := unix.Flock(int(file.Fd()), unix.LOCK_EX|unix.LOCK_NB); err != nil {
		file.Close()
		return nil, err
	}
	return file, nil
}
