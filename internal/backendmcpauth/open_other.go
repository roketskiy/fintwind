//go:build !windows

package backendmcpauth

import "errors"

func openSystemURL(string) error {
	return errors.New("system authorization browser is unavailable on this platform")
}
