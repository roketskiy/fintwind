package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// claudequotacheck is a private E2E harness. When copied as claude.exe into a
// round-owned bin directory, this same binary is the only CLI the test starts.
func main() {
	if strings.EqualFold(filepath.Base(os.Args[0]), "claude.exe") || len(os.Args) > 1 && (os.Args[1] == "auth" || os.Args[1] == "fake-child" || os.Args[1] == "shim-record" || os.Args[1] == "--setting-sources") {
		if err := fakeMain(os.Args[1:]); err != nil {
			fmt.Fprintln(os.Stderr, "[X] fake Claude:", err)
			os.Exit(2)
		}
		return
	}
	daemon := flag.String("daemon", "", "absolute isolated Go daemon executable")
	out := flag.String("out", "", "report output directory")
	directOnboarding := flag.Bool("direct-onboarding", false, "run only the isolated two-stage raw ConPTY onboarding diagnostic")
	directRedraw := flag.Bool("direct-redraw-diagnostic", false, "capture fake ConPTY output after complete onboarding and /usage")
	shimEscapeDiagnostic := flag.Bool("shim-escape-diagnostic", false, "compare single/double CMD escaping under an owned Job")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()
	if *directRedraw {
		if err := runDirectRedrawDiagnostic(ctx, *out); err != nil {
			fmt.Fprintln(os.Stderr, "[X] direct redraw diagnostic:", err)
			os.Exit(1)
		}
		return
	}
	if *shimEscapeDiagnostic {
		if err := runShimEscapeDiagnostic(ctx, *out); err != nil {
			fmt.Fprintln(os.Stderr, "[X] shim escape diagnostic:", err)
			os.Exit(1)
		}
		return
	}
	if *directOnboarding {
		if err := runDirectOnboarding(ctx, *out); err != nil {
			fmt.Fprintln(os.Stderr, "[X] direct onboarding:", err)
			os.Exit(1)
		}
		return
	}
	if err := run(ctx, *daemon, *out); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}
