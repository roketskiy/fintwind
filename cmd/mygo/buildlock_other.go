//go:build !windows

package main

import (
	"context"
	"errors"
)

func buildLock(context.Context, string) (func(), error) {
	return nil, errors.New("MyGo launcher requires Windows Job Objects")
}
