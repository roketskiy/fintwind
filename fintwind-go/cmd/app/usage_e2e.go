//go:build usagee2e

package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/ELY-Open-Source/CrychicUI/sound"
)

type usageE2EStep struct {
	Name   string `json:"name"`
	Passed bool   `json:"passed"`
	Detail any    `json:"detail"`
}

type usageE2EReport struct {
	Passed               bool           `json:"passed"`
	FrontendLaunched     bool           `json:"frontendLaunched"`
	LiveProviderVerified bool           `json:"liveProviderVerified"`
	Scope                string         `json:"scope"`
	Limitations          []string       `json:"limitations"`
	Steps                []usageE2EStep `json:"steps"`
}

func usageE2EBaseReport() usageE2EReport {
	return usageE2EReport{
		Scope: "loopback IPC fixture, production decoding/aggregation, headless usage UI semantics",
		Limitations: []string{
			"No native window, screenshots, image artifacts, live daemon, provider, credentials, or real profile are used.",
			"Window.Update background result application is not exercised. Fixture responses are decoded and aggregated by production code, then installed in app.usage for headless UI input.",
			"Background stale-generation rejection, automatic refresh completion, and scheduled midnight cache application are not verified. Time-controlled DST and year-rollover cases exercise the production aggregate rebuild pipeline only.",
			"Headless geometry and accessibility checks do not establish visual parity or native platform accessibility behavior.",
		},
	}
}

func (r *usageE2EReport) check(name string, expected, observed any) {
	want, wantErr := json.Marshal(expected)
	got, gotErr := json.Marshal(observed)
	r.Steps = append(r.Steps, usageE2EStep{
		Name: name, Passed: wantErr == nil && gotErr == nil && string(want) == string(got),
		Detail: map[string]any{"expected": expected, "observed": observed},
	})
}

func init() {
	SetLanguage("english")
	sound.SetEnabled(false)
	dir := os.Getenv("FINTWIND_USAGE_E2E_DIR")
	if dir == "" {
		fmt.Fprintln(os.Stderr, "usagee2e requires FINTWIND_USAGE_E2E_DIR; the frontend will not launch")
		os.Exit(2)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	results := make(chan usageE2EReport, 1)
	go func() {
		r := usageE2EBaseReport()
		defer func() {
			if problem := recover(); problem != nil {
				r.Steps = append(r.Steps, usageE2EStep{Name: "unhandled panic", Detail: fmt.Sprint(problem)})
			}
			results <- r
		}()
		usageE2ERun(ctx, &r)
	}()
	var report usageE2EReport
	select {
	case report = <-results:
	case <-ctx.Done():
		report = usageE2EBaseReport()
		report.Steps = append(report.Steps, usageE2EStep{Name: "overall deadline", Detail: ctx.Err().Error()})
	}
	report.Passed = len(report.Steps) > 0
	for _, step := range report.Steps {
		report.Passed = report.Passed && step.Passed
	}
	data, err := json.MarshalIndent(report, "", "  ")
	if err == nil {
		err = os.MkdirAll(dir, 0o700)
	}
	if err == nil {
		err = os.WriteFile(filepath.Join(dir, "usage-e2e.json"), append(data, '\n'), 0o600)
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	if !report.Passed {
		fmt.Fprintln(os.Stderr, "usagee2e failed; see", filepath.Join(dir, "usage-e2e.json"))
		os.Exit(1)
	}
	fmt.Fprintln(os.Stdout, "usagee2e passed; see", filepath.Join(dir, "usage-e2e.json"))
	os.Exit(0)
}
