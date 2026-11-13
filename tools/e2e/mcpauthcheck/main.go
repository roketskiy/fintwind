package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"time"
)

// mcpauthcheck is an isolated E2E for MCP OAuth authorization. It never runs
// a real `mcp auth`, never opens a browser, never touches real credentials:
// the daemon under test launches this same executable as its OpenCode CLI
// (argv `mcp auth <name>`), and the fake below only prints fixture output
// driven by a JSON seed. No URL printed by the fake is ever opened: the
// daemon layer never prints https at all, and the module layer uses a
// recording opener.
func main() {
	if len(os.Args) > 1 && os.Args[1] == "mcp" {
		if err := fakeMain(os.Args[2:]); err != nil {
			fmt.Fprintln(os.Stderr, "[X]", err)
			os.Exit(1)
		}
		return
	}
	if len(os.Args) > 1 && os.Args[1] == "mcp-fake-hold" {
		holdMain(os.Args[2:])
		return
	}
	if len(os.Args) == 2 && os.Args[1] == "--version" {
		fmt.Println("mcpauthcheck 0.0.0")
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
