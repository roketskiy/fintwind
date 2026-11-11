package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"

	"fintwind-go/internal/ownedprocess"
)

func main() {
	app := flag.String("app", "", "path to the usagee2e-built app executable")
	out := flag.String("out", "", "directory for usage-e2e.json")
	flag.Parse()
	if *app == "" || *out == "" {
		fmt.Fprintln(os.Stderr, "usageverify requires -app <usagee2e.exe> -out <directory>")
		os.Exit(2)
	}
	if err := run(*app, *out); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(executable, output string) (result error) {
	if err := os.MkdirAll(output, 0o700); err != nil {
		return err
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	fmt.Println("round directory:", round)
	var child *ownedprocess.Owned
	var childExited bool
	defer func() {
		if child != nil {
			result = errors.Join(result, child.Close())
		}
		data, readErr := os.ReadFile(filepath.Join(round, "usage-e2e.json"))
		var report map[string]any
		if readErr == nil {
			readErr = json.Unmarshal(data, &report)
		}
		if readErr != nil || report == nil {
			result = errors.Join(result, fmt.Errorf("child report unavailable: %v", readErr))
			report = map[string]any{"frontendLaunched": false, "liveProviderVerified": false, "steps": []any{}}
		}
		if result != nil {
			report["passed"] = false
			steps, _ := report["steps"].([]any)
			report["steps"] = append(steps, map[string]any{"name": "owned supervisor", "passed": false, "detail": result.Error()})
		}
		report["supervisor"] = map[string]any{"roundDir": round, "stdoutPath": filepath.Join(round, "child-stdout.log"), "stderrPath": filepath.Join(round, "child-stderr.log"), "isolatedEnvironment": child != nil, "killOnCloseJob": child != nil, "childExited": childExited}
		data, marshalErr := json.MarshalIndent(report, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(output, "usage-e2e.json"), append(data, '\n'), 0o600)
		}
		result = errors.Join(result, marshalErr)
	}()
	child, err = ownedprocess.Start(executable, nil, round, map[string]string{"FINTWIND_USAGE_E2E_DIR": round})
	if err != nil {
		return err
	}
	fmt.Println("child stdout:", child.StdoutPath)
	fmt.Println("child stderr:", child.StderrPath)
	childExited, err = child.Exited(50000)
	if err != nil {
		return err
	}
	if !childExited {
		return errors.New("usagee2e child exceeded the 50 second deadline")
	}
	data, err := os.ReadFile(filepath.Join(round, "usage-e2e.json"))
	if err != nil {
		return err
	}
	var report struct {
		Passed               *bool `json:"passed"`
		FrontendLaunched     *bool `json:"frontendLaunched"`
		LiveProviderVerified *bool `json:"liveProviderVerified"`
		Steps                []struct {
			Name   string `json:"name"`
			Passed *bool  `json:"passed"`
		} `json:"steps"`
	}
	if err := json.Unmarshal(data, &report); err != nil {
		return err
	}
	if report.Passed == nil || !*report.Passed || report.FrontendLaunched == nil || *report.FrontendLaunched || report.LiveProviderVerified == nil || *report.LiveProviderVerified || len(report.Steps) == 0 {
		return errors.New("usagee2e child did not provide a passing headless fixture report")
	}
	for _, step := range report.Steps {
		if step.Name == "" || step.Passed == nil || !*step.Passed {
			return fmt.Errorf("usagee2e step failed: %s", step.Name)
		}
	}
	return nil
}
