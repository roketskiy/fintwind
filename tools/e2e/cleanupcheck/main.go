package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"time"
)

// cleanupcheck is an isolated E2E for previewSessionCleanup / cleanupOldSession.
// It never touches production data and never uses a real provider or model:
// the daemon under test launches this same executable as its OpenCode binary,
// and the serve subcommand emulates only the native session API the cleanup
// path uses (paged listing, active set, re-fetch, recursive delete root).
func main() {
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		if err := serveFixture(os.Args[2:]); err != nil {
			fmt.Fprintln(os.Stderr, "[X]", err)
			os.Exit(1)
		}
		return
	}
	if len(os.Args) == 2 && os.Args[1] == "--version" {
		fmt.Println("cleanup-fixture 0.0.0")
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
