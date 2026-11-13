package main

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/backendremote"
)

func run() error {
	if len(os.Args) != 2 {
		return fmt.Errorf("local relay address required")
	}
	directory, err := os.MkdirTemp("", "fintwind-remote-identity-check-")
	if err != nil {
		return err
	}
	identity, err := backendremote.LoadIdentity(directory)
	if err != nil {
		return err
	}
	again, err := backendremote.LoadIdentity(directory)
	if err != nil || again.InstallationID() != identity.InstallationID() {
		return fmt.Errorf("identity changed across reload")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	enrollment := backendremote.Enrollment{Origin: os.Args[1], Client: &http.Client{Timeout: 5 * time.Second}, Identity: identity}
	if _, err := enrollment.Token(ctx); err != nil {
		return err
	}
	path := filepath.Join(directory, "remote-control-identity.bin")
	if err := os.WriteFile(path, []byte("damaged"), 0600); err != nil {
		return err
	}
	if _, err := backendremote.LoadIdentity(directory); err == nil {
		return fmt.Errorf("damaged identity was replaced")
	}
	fmt.Println("local admission and identity persistence verified")
	return nil
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
