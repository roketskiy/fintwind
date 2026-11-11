package main

import (
	"fmt"
	"os"
	"path/filepath"
	"time"

	"golang.org/x/sys/windows"
)

// redirectCrashLog appends the process's stderr — Go panics and runtime
// fatals — to crash.log beside the app settings. A GUI app has no console,
// so without this a window death leaves nothing behind; the log keeps the
// stack for the next one. Best effort: any failure keeps stderr as is.
func redirectCrashLog() {
	root := os.Getenv("LOCALAPPDATA")
	if root == "" {
		return
	}
	dir := filepath.Join(root, "Fintwind Go")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return
	}
	file, err := os.OpenFile(filepath.Join(dir, "crash.log"), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return
	}
	fmt.Fprintf(file, "\n=== launch %s pid %d ===\n", time.Now().Format("2006-01-02 15:04:05"), os.Getpid())
	if err := windows.SetStdHandle(windows.STD_ERROR_HANDLE, windows.Handle(file.Fd())); err != nil {
		file.Close()
		return
	}
	// Kept open for the process lifetime: the runtime writes panics to OS
	// handle 2, not to the os.Stderr variable.
}
