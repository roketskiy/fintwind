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
	app := flag.String("app", "", "path to wiringe2e-built app")
	out := flag.String("out", "", "new output directory")
	flag.Parse()
	if *app == "" || *out == "" {
		fmt.Fprintln(os.Stderr, "wiringverify requires -app <tagged.exe> -out <unique-directory>")
		os.Exit(2)
	}
	if err := run(*app, *out); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(executable, output string) (result error) {
	if err := os.Mkdir(output, 0o700); err != nil {
		return fmt.Errorf("output must be new: %w", err)
	}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	fmt.Println("round directory:", round)
	var child *ownedprocess.Owned
	var exited bool
	defer func() {
		if child != nil {
			result = errors.Join(result, child.Close())
		}
		data, readErr := os.ReadFile(filepath.Join(round, "full-wiring-e2e.json"))
		var report map[string]any
		if readErr == nil {
			readErr = json.Unmarshal(data, &report)
		}
		if readErr != nil || report == nil {
			result = errors.Join(result, fmt.Errorf("child report unavailable: %v", readErr))
			report = map[string]any{"passed": false, "frontendLaunched": false, "liveProviderVerified": false, "steps": []any{}}
		}
		if result != nil {
			report["passed"] = false
			steps, _ := report["steps"].([]any)
			report["steps"] = append(steps, map[string]any{"name": "owned supervisor", "passed": false, "expected": "owned child completes", "observed": result.Error()})
		}
		report["supervisor"] = map[string]any{"roundDir": round, "stdoutPath": filepath.Join(round, "child-stdout.log"), "stderrPath": filepath.Join(round, "child-stderr.log"), "isolatedEnvironment": child != nil, "killOnCloseJob": child != nil, "childExited": exited, "maxChildWaitSeconds": 75}
		data, err := json.MarshalIndent(report, "", "  ")
		if err == nil {
			err = os.WriteFile(filepath.Join(output, "full-wiring-e2e.json"), append(data, '\n'), 0o600)
		}
		result = errors.Join(result, err)
	}()
	child, err = ownedprocess.Start(executable, nil, round, map[string]string{"FINTWIND_WIRING_E2E_DIR": round})
	if err != nil {
		return err
	}
	exited, err = child.Exited(75000)
	if err != nil {
		return err
	}
	if !exited {
		return errors.New("wiring child exceeded 75 seconds")
	}
	data, err := os.ReadFile(filepath.Join(round, "full-wiring-e2e.json"))
	if err != nil {
		return err
	}
	var report struct {
		Passed   *bool `json:"passed"`
		Frontend *bool `json:"frontendLaunched"`
		Provider *bool `json:"liveProviderVerified"`
		Steps    []struct {
			Name   string
			Passed bool
		}
	}
	if err := json.Unmarshal(data, &report); err != nil {
		return err
	}
	if report.Passed == nil || !*report.Passed || report.Frontend == nil || *report.Frontend || report.Provider == nil || *report.Provider || len(report.Steps) == 0 {
		return errors.New("wiring fixture report did not pass")
	}
	for _, step := range report.Steps {
		if step.Name == "" || !step.Passed {
			return fmt.Errorf("wiring step failed: %s", step.Name)
		}
	}
	return nil
}
