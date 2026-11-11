//go:build wiringe2e

package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/daemon"

	"github.com/ELY-Open-Source/CrychicUI/sound"
)

type wiringStep struct {
	Name     string `json:"name"`
	Passed   bool   `json:"passed"`
	Expected any    `json:"expected"`
	Observed any    `json:"observed"`
}

type wiringReport struct {
	Passed               bool         `json:"passed"`
	FrontendLaunched     bool         `json:"frontendLaunched"`
	LiveProviderVerified bool         `json:"liveProviderVerified"`
	VisualTestPerformed  bool         `json:"visualTestPerformed"`
	Scope                string       `json:"scope"`
	Limitations          []string     `json:"limitations"`
	Steps                []wiringStep `json:"steps"`
}

func (r *wiringReport) check(name string, expected, observed any) {
	want, e1 := json.Marshal(expected)
	got, e2 := json.Marshal(observed)
	r.Steps = append(r.Steps, wiringStep{name, e1 == nil && e2 == nil && string(want) == string(got), expected, observed})
}

func wiringBaseReport() wiringReport {
	return wiringReport{Scope: fmt.Sprintf("protocol %d loopback fixture, production typed models and headless input, isolated real Git commands and local preferences", daemon.ProtocolVersion), Limitations: []string{
		"No native window, screenshot, image artifact, live daemon, provider, credentials, real profile or real scheduled task is used.",
		"Selected scenarios run production app.post callbacks on the headless Tester owner thread. Native Window.Update and platform event delivery are not exercised.",
		"Fixture payloads use production decoding, then the driver directly installs typed state for headless input. This is partial integration coverage, not full native end-to-end acceptance.",
		"Mobile bridge backend, SSH, tunnels, live provider/profile mutations and scheduled execution are excluded. OAuth and account switching use a loopback fixture.",
		"Headless accessibility observations do not establish visual parity or native platform accessibility.",
	}}
}

func init() {
	SetLanguage("english")
	sound.SetEnabled(false)
	dir := os.Getenv("FINTWIND_WIRING_E2E_DIR")
	if dir == "" {
		fmt.Fprintln(os.Stderr, "wiringe2e requires FINTWIND_WIRING_E2E_DIR; GUI main is disabled")
		os.Exit(2)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 70*time.Second)
	defer cancel()
	results := make(chan wiringReport, 1)
	go func() {
		r := wiringBaseReport()
		defer func() {
			if issue := recover(); issue != nil {
				r.check("unhandled panic", "none", fmt.Sprint(issue))
			}
			results <- r
		}()
		wiringRun(ctx, &r, dir)
	}()
	var report wiringReport
	select {
	case report = <-results:
	case <-ctx.Done():
		report = wiringBaseReport()
		report.check("overall deadline", "completed within 70 seconds", ctx.Err().Error())
	}
	report.Passed = len(report.Steps) > 0
	for _, step := range report.Steps {
		report.Passed = report.Passed && step.Passed
	}
	data, err := json.MarshalIndent(report, "", "  ")
	if err == nil {
		err = os.WriteFile(filepath.Join(dir, "full-wiring-e2e.json"), append(data, '\n'), 0o600)
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Fprintln(os.Stdout, "wiring report:", filepath.Join(dir, "full-wiring-e2e.json"))
	if !report.Passed {
		os.Exit(1)
	}
	os.Exit(0)
}
