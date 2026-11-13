//go:build !windows

package main

import (
	"context"
	"errors"
)

func runPreferenceAwake(ctx context.Context, report func(error)) {
	report(errors.New("Not supported on this platform"))
}
