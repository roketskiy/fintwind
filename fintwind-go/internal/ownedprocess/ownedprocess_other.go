//go:build !windows

package ownedprocess

import "errors"

// Start is unsupported off Windows: there is no Job Object semantic to
// contain grandchildren, and this package offers no unsafe fallback.
func Start(executable string, args []string, roundDir string, extra map[string]string) (*Owned, error) {
	return nil, errors.New("ownedprocess: Windows Job Objects required")
}

func StartWithEnvironment(executable string, args []string, runDir, workingDir string, environment []string) (*Owned, error) {
	return nil, errors.New("ownedprocess: Windows Job Objects required")
}
