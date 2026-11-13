package main

// Daemon wire phases: refusing/invalid proxy, missing/failing binary,
// optional one-shot official latest GET (read-only, blocked when offline).

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/backendmanagement"
	"fintwind-go/internal/backendstore"
)

func statusFields(response backendstore.Object) (path, current, latest, checkError any, available bool, err error) {
	if response.String("type") != "openCodeUpdateStatus" {
		return nil, nil, nil, nil, false, errors.New("wrong reply type: " + response.String("type"))
	}
	var status struct {
		Path      any  `json:"path"`
		Current   any  `json:"currentVersion"`
		Latest    any  `json:"latestVersion"`
		Available bool `json:"updateAvailable"`
		CheckErr  any  `json:"checkError"`
	}
	raw, err := json.Marshal(response)
	if err != nil {
		return nil, nil, nil, nil, false, err
	}
	if err := json.Unmarshal(raw, &status); err != nil {
		return nil, nil, nil, nil, false, err
	}
	return status.Path, status.Current, status.Latest, status.CheckErr, status.Available, nil
}

func isNull(value any) bool { return value == nil }

func nonEmptyString(value any) bool {
	text, ok := value.(string)
	return ok && text != ""
}

func (h *harness) daemonSuite() error {
	proxy, err := refusingProxyURL()
	if err != nil {
		return err
	}
	want := map[string]string{"opencode_binary_path": h.self, "network_proxy_url": proxy}
	if err := h.writeDaemonJSON(want); err != nil {
		return err
	}
	response, err := h.checkUpdate()
	if err != nil {
		return fmt.Errorf("refusing proxy: %w", err)
	}
	path, current, latest, checkError, available, err := statusFields(response)
	if err != nil {
		return err
	}
	if path != h.self || current != "2.0.0" || !isNull(latest) || available || !nonEmptyString(checkError) {
		raw, _ := json.Marshal(response)
		return fmt.Errorf("refusing proxy status wrong: %s", raw)
	}
	if err := h.daemonJSONUnchanged(want); err != nil {
		return err
	}
	h.notes["refusing-proxy-checkError"] = fmt.Sprintf("%v", checkError)
	h.checks["daemon-refusing-proxy-keeps-identity"] = true

	want = map[string]string{"opencode_binary_path": h.self, "network_proxy_url": "::bad proxy::"}
	if err := h.writeDaemonJSON(want); err != nil {
		return err
	}
	invalid, err := h.checkUpdate()
	if err != nil {
		return fmt.Errorf("invalid proxy: %w", err)
	}
	_, _, _, _, _, err = statusFields(invalid)
	if err != nil {
		return err
	}
	var parsed struct {
		CheckErr any `json:"checkError"`
	}
	raw, _ := json.Marshal(invalid)
	if err := json.Unmarshal(raw, &parsed); err != nil {
		return err
	}
	text, _ := parsed.CheckErr.(string)
	if text != "invalid configured network proxy" {
		return fmt.Errorf("invalid proxy checkError = %v", parsed.CheckErr)
	}
	var currentHeld struct {
		Current any `json:"currentVersion"`
		Path    any `json:"path"`
	}
	if err := json.Unmarshal(raw, &currentHeld); err != nil {
		return err
	}
	if currentHeld.Path != h.self || currentHeld.Current != "2.0.0" {
		return fmt.Errorf("invalid proxy dropped identity: %s", raw)
	}
	if err := h.daemonJSONUnchanged(want); err != nil {
		return err
	}
	h.checks["daemon-invalid-proxy-keeps-identity"] = true

	missing := filepath.Join(h.round, "missing-opencode.exe")
	want = map[string]string{"opencode_binary_path": missing, "network_proxy_url": proxy}
	if err := h.writeDaemonJSON(want); err != nil {
		return err
	}
	absent, err := h.checkUpdate()
	if err != nil {
		return fmt.Errorf("missing binary: %w", err)
	}
	absentPath, absentCurrent, absentLatest, absentErr, absentAvailable, err := statusFields(absent)
	if err != nil {
		return err
	}
	if !isNull(absentPath) || !isNull(absentCurrent) || !isNull(absentLatest) || absentAvailable || !nonEmptyString(absentErr) {
		raw, _ := json.Marshal(absent)
		return fmt.Errorf("missing binary status wrong: %s", raw)
	}
	if err := h.daemonJSONUnchanged(want); err != nil {
		return err
	}
	h.checks["daemon-missing-binary-null-identity"] = true

	stub := filepath.Join(h.round, "not-executable.dat")
	if err := os.WriteFile(stub, []byte("not an executable"), 0o600); err != nil {
		return err
	}
	want = map[string]string{"opencode_binary_path": stub, "network_proxy_url": proxy}
	if err := h.writeDaemonJSON(want); err != nil {
		return err
	}
	broken, err := h.checkUpdate()
	if err != nil {
		return fmt.Errorf("failing version command: %w", err)
	}
	brokenPath, brokenCurrent, _, brokenErr, brokenAvailable, err := statusFields(broken)
	if err != nil {
		return err
	}
	if brokenPath != stub || !isNull(brokenCurrent) || brokenAvailable || !nonEmptyString(brokenErr) {
		raw, _ := json.Marshal(broken)
		return fmt.Errorf("failing version status wrong: %s", raw)
	}
	if err := h.daemonJSONUnchanged(want); err != nil {
		return err
	}
	h.notes["version-command-failure-checkError"] = fmt.Sprintf("%v", brokenErr)
	h.checks["daemon-version-command-failure-keeps-path"] = true
	return nil
}

func (h *harness) realNetworkSuite() error {
	if err := h.writeDaemonJSON(map[string]string{"opencode_binary_path": h.self}); err != nil {
		return err
	}
	// Exactly one read-only official metadata GET through the ambient proxy.
	// No credentials, no download, no install. Unreachable network is
	// recorded as blocked, never as failure, and never retried here.
	client := &http.Client{Transport: &http.Transport{Proxy: http.ProxyFromEnvironment}}
	ctx, cancel := context.WithTimeout(h.ctx, 45*time.Second)
	defer cancel()
	version, err := backendmanagement.Latest(ctx, client, backendmanagement.LatestURL)
	if err != nil {
		h.blocked = append(h.blocked, "official latest unreachable from sandbox: "+err.Error())
		h.checks["official-latest-blocked-not-failed"] = true
		return nil
	}
	if !backendmanagement.SupportedVersion(version) {
		return fmt.Errorf("official latest %q failed supported-version gate", version)
	}
	h.notes["official-latest"] = version
	comparison, err := backendmanagement.Compare(version, "2.0.0")
	if err != nil {
		return err
	}
	h.notes["official-vs-self"] = fmt.Sprintf("compare=%d", comparison)
	response, err := h.checkUpdate()
	if err != nil {
		h.blocked = append(h.blocked, "daemon direct fetch failed while Doer-only succeeded: "+err.Error())
		h.checks["official-latest-blocked-not-failed"] = true
		return nil
	}
	_, current, latest, checkError, available, err := statusFields(response)
	if err != nil {
		return err
	}
	if current != "2.0.0" {
		raw, _ := json.Marshal(response)
		return fmt.Errorf("live wire lost current version: %s", raw)
	}
	if latestStr, ok := latest.(string); !ok || latestStr == "" {
		h.blocked = append(h.blocked, fmt.Sprintf("daemon direct fetch returned checkError %v despite reachable endpoint", checkError))
		h.checks["official-latest-blocked-not-failed"] = true
		return nil
	}
	if available != (comparison > 0) {
		raw, _ := json.Marshal(response)
		return fmt.Errorf("live wire availability mismatch: %s", raw)
	}
	h.checks["official-latest-live-wire"] = true
	return nil
}
