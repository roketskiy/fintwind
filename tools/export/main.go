package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"time"

	"fintwind-go/internal/backendstore"
)

func main() {
	source := flag.String("source", "", "absolute source SQLite database, read-only")
	output := flag.String("output", "", "new directory for sanitized export")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	report, err := backendstore.ExportSanitized(ctx, *source, *output)
	if err != nil {
		fmt.Fprintln(os.Stderr, "[X] Sanitized export failed; no source contents logged")
		os.Exit(1)
	}
	fmt.Printf("[OK] Sanitized export: %d projects, %d sessions\n", report.Projects, report.Sessions)
}
