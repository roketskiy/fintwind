//go:build !windows

package backendskills

import "errors"

func trashDirectory(string) error {
	return errors.New("system trash is not available on this platform")
}
