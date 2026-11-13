//go:build unix

package backendprivate

import "os"

func Directory(path string) error { return os.Chmod(path, 0o700) }
