package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"

	"fintwind-go/internal/backendattachments"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/ownedprocess"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}
func run() (err error) {
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	defer func() {
		raw, _ := json.MarshalIndent(map[string]any{"checks": checks, "passed": err == nil}, "", "  ")
		_ = os.WriteFile(filepath.Join(round, "attachment-result.json"), raw, 0o600)
		fmt.Println("[REPORT]", filepath.Join(round, "attachment-result.json"))
	}()
	workspace := filepath.Join(round, "workspace")
	if err := os.Mkdir(workspace, 0o700); err != nil {
		return err
	}
	service := backendattachments.New(filepath.Join(round, "data"), workspace, false, nil)
	call := func(value map[string]any) (backendstore.Object, error) {
		raw, _ := json.Marshal(value)
		result, handled, err := service.Handle(context.Background(), raw)
		if !handled && err == nil {
			return nil, errors.New("not handled")
		}
		if err != nil {
			return nil, err
		}
		return backendstore.Decode(result)
	}
	blob, err := call(map[string]any{"type": "storeBlob", "mimeType": "image/png", "bytes": base64.StdEncoding.EncodeToString([]byte("literal bytes"))})
	if err != nil {
		return err
	}
	read, err := call(map[string]any{"type": "readBlob", "reference": blob.String("reference")})
	if err != nil {
		return err
	}
	if read.String("bytes") != base64.StdEncoding.EncodeToString([]byte("literal bytes")) {
		return errors.New("blob mismatch")
	}
	checks["blob-binary-roundtrip"] = true
	if _, err := call(map[string]any{"type": "readBlob", "reference": "fintwind-blob:../escape"}); err == nil {
		return errors.New("blob escape accepted")
	}
	checks["blob-escape-refused"] = true
	attachment, err := call(map[string]any{"type": "importAttachment", "name": "fixture.txt", "upload": map[string]string{"kind": "file", "data_base64": base64.StdEncoding.EncodeToString([]byte("literal file"))}})
	if err != nil {
		return err
	}
	stored, err := backendstore.Decode(attachment["attachment"])
	if err != nil {
		return err
	}
	read, err = call(map[string]any{"type": "readAttachment", "reference": stored.String("reference"), "path": stored.String("path")})
	if err != nil {
		return err
	}
	if read.String("bytes") != base64.StdEncoding.EncodeToString([]byte("literal file")) {
		return errors.New("attachment mismatch")
	}
	checks["attachment-roundtrip"] = true
	if _, err := call(map[string]any{"type": "importAttachment", "name": "dir", "upload": map[string]any{"kind": "directory", "entries": []any{map[string]string{"relativePath": "../bad", "dataBase64": "eA=="}}}}); err == nil {
		return errors.New("directory escape accepted")
	}
	checks["directory-escape-refused"] = true
	if _, err := call(map[string]any{"type": "sweepBlobs"}); err == nil {
		return errors.New("sweep without live reference source accepted")
	}
	checks["sweep-fails-closed"] = true
	return nil
}
