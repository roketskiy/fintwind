package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"time"
)

// updatecheck is an isolated E2E for the read-only checkOpenCodeUpdate path.
// It never downloads, installs, or upgrades any OpenCode binary, never sends
// credentials, and never writes daemon configuration. Latest-metadata cases
// run Latest over real loopback sockets against httptest servers (no mocked
// Doer); the daemon wire is driven over a real WS connection.
func main() {
	if len(os.Args) == 2 && os.Args[1] == "--version" {
		fmt.Println("updatecheck 2.0.0")
		return
	}
	daemon := flag.String("daemon", "", "absolute Go daemon binary")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()
	if err := run(ctx, *daemon); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}
