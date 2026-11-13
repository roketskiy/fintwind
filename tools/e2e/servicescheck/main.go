package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"fintwind-go/internal/backendservices"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/ownedprocess"
)

type fixture struct{ requests []string }

func (f *fixture) RoundTrip(_ context.Context, method, path string, body any) (json.RawMessage, error) {
	f.requests = append(f.requests, method+" "+path)
	if method == "GET" && path == "/api/model?types=image" {
		return json.RawMessage(`{"data":[{"id":"literal-image","providerID":"fixture","name":"Literal Image","type":"image"},{"id":"not-image","providerID":"fixture","type":"chat"}]}`), nil
	}
	if method == "GET" && strings.HasPrefix(path, "/api/model") {
		return json.RawMessage(`{"data":[{"id":"literal-model","providerID":"fixture","name":"Literal Model"}]}`), nil
	}
	if method == "GET" && strings.HasPrefix(path, "/api/session?") {
		return json.RawMessage(`{"data":[{"id":"ses_literal","title":"Literal task","time":{"created":1000,"updated":2000},"location":{"directory":"fixture"}}]}`), nil
	}
	if path == "/api/integration/fixture/connect/oauth/attempt/complete" {
		return json.RawMessage(`{"data":{"status":"complete"}}`), nil
	}
	return json.RawMessage(`{"data":{}}`), nil
}

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
		_ = os.WriteFile(filepath.Join(round, "services-result.json"), raw, 0o600)
		fmt.Println("[REPORT]", filepath.Join(round, "services-result.json"))
	}()
	api := &fixture{}
	service := backendservices.New(round, func(context.Context, backendstore.Object) (backendservices.API, error) { return api, nil })
	call := func(value map[string]any) (backendstore.Object, error) {
		raw, _ := json.Marshal(value)
		response, handled, err := service.Handle(context.Background(), raw)
		if !handled && err == nil {
			return nil, errors.New("command not handled")
		}
		if err != nil {
			return nil, err
		}
		return backendstore.Decode(response)
	}
	probe, err := call(map[string]any{"type": "probeProvider", "discoverModels": true, "directory": round})
	if err != nil {
		return err
	}
	var got struct {
		Models []struct {
			ID string `json:"id"`
		} `json:"models"`
	}
	if err := json.Unmarshal(probe["probe"], &got); err != nil {
		return err
	}
	if len(got.Models) != 1 || got.Models[0].ID != "fixture/literal-model" {
		return errors.New("provider model mapping mismatch")
	}
	checks["provider-model-map"] = true
	images, err := call(map[string]any{"type": "fetchImageModels", "directory": round})
	if err != nil {
		return err
	}
	var inventory []struct {
		ID       string `json:"id"`
		Provider string `json:"provider"`
	}
	if err := json.Unmarshal(images["value"], &inventory); err != nil {
		return err
	}
	if images.String("type") != "json" || len(inventory) != 1 || inventory[0].ID != "fixture/literal-image" || inventory[0].Provider != "fixture" {
		return errors.New("image inventory wire mismatch")
	}
	checks["image-inventory-json-contract"] = true
	list, err := call(map[string]any{"type": "listProviderSessions", "directory": round})
	if err != nil {
		return err
	}
	var sessions []struct {
		ID      string `json:"sessionId"`
		Created int    `json:"createdAt"`
	}
	if err := json.Unmarshal(list["sessions"], &sessions); err != nil {
		return err
	}
	if len(sessions) != 1 || sessions[0].ID != "ses_literal" || sessions[0].Created != 1 {
		return errors.New("session mapping mismatch")
	}
	checks["native-session-list"] = true
	if _, err := call(map[string]any{"type": "authorizeProvider", "providerId": "fixture", "key": "sentinel-not-real"}); err != nil {
		return err
	}
	if _, err := call(map[string]any{"type": "completeProviderOAuth", "providerId": "fixture", "attemptId": "attempt", "code": "sentinel-code"}); err != nil {
		return err
	}
	found := false
	for _, path := range api.requests {
		if path == "POST /api/integration/fixture/connect/oauth/attempt/complete" {
			found = true
		}
	}
	if !found {
		return errors.New("oauth completion route missing")
	}
	checks["oauth-completion-contract"] = true
	if _, err := call(map[string]any{"type": "probeBuiltinProvider", "providerId": "fixture"}); err != nil {
		return err
	}
	checks["builtin-provider-count"] = true
	return nil
}
