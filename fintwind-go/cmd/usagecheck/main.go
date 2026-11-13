package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"time"
)

// usagecheck is an isolated E2E for fetchUsageStats and the usage index.
// No real models, no credentials, no pricing network, no production data:
// the daemon under test launches this same executable as its OpenCode engine,
// and the serve subcommand emulates only the session listing and message
// history APIs the usage scan uses, driven by a JSON seed the test rewrites
// per phase. Verdicts come from executed code with a repeatable JSON report.
func main() {
	if len(os.Args) > 1 && os.Args[1] == "serve" {
		if err := serveFixture(os.Args[2:]); err != nil {
			fmt.Fprintln(os.Stderr, "[X]", err)
			os.Exit(1)
		}
		return
	}
	if len(os.Args) == 2 && os.Args[1] == "--version" {
		fmt.Println("usagecheck 0.0.0")
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
