//go:build !windows

package main

import "errors"

func containCurrentProcess() error {
	return errors.New("Windows Job Object containment is required")
}
