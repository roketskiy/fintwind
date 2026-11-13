//go:build !windows

package backendworkspace

import "os"

func discoveryLink(info os.FileInfo) bool { return info.Mode()&os.ModeSymlink != 0 }
