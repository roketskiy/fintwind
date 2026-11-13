package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/ownedprocess"
)

const maxSpecBytes = 1 << 20
const maxChildTimeoutMS = 4 * 60 * 60 * 1000

var deniedEnvironment = map[string]struct{}{
	"FINTWIND_AGENT_REAL_PROVIDER": {}, "DEEPSEEK_API_KEY": {}, "OPENAI_API_KEY": {},
	"ANTHROPIC_API_KEY": {}, "GEMINI_API_KEY": {}, "CLAUDE_CODE_OAUTH_TOKEN": {},
	"GH_TOKEN": {}, "GITHUB_TOKEN": {}, "GH_ENTERPRISE_TOKEN": {}, "OPENCODE_API_KEY": {},
	"GOOGLE_API_KEY": {}, "GOOGLE_APPLICATION_CREDENTIALS": {}, "AZURE_OPENAI_API_KEY": {},
	"HF_TOKEN": {}, "GIT_ASKPASS": {}, "SSH_ASKPASS": {}, "HTTP_PROXY": {}, "HTTPS_PROXY": {},
	"ALL_PROXY": {}, "NO_PROXY": {},
}

type commandSpec struct {
	Executable           string            `json:"executable"`
	Arguments            []string          `json:"arguments"`
	Environment          map[string]string `json:"environment"`
	EnvironmentAllowlist []string          `json:"environmentAllowlist"`
	WorkingDirectory     string            `json:"workingDirectory"`
	AllowedRunRoot       string            `json:"allowedRunRoot"`
	RunDirectory         string            `json:"runDirectory"`
	ReadyPath            string            `json:"readyPath"`
	AbortPath            string            `json:"abortPath"`
	ResultPath           string            `json:"resultPath"`
	TimeoutMilliseconds  int64             `json:"timeoutMilliseconds"`
}

type runResult struct {
	Status           string   `json:"status"`
	Contained        bool     `json:"contained"`
	CommandStarted   bool     `json:"commandStarted"`
	Executable       string   `json:"executable,omitempty"`
	Arguments        []string `json:"arguments,omitempty"`
	WorkingDirectory string   `json:"workingDirectory,omitempty"`
	EnvironmentKeys  []string `json:"environmentKeys,omitempty"`
	StdoutPath       string   `json:"stdoutPath,omitempty"`
	StderrPath       string   `json:"stderrPath,omitempty"`
	ExitCode         *uint32  `json:"exitCode,omitempty"`
	TimedOut         bool     `json:"timedOut"`
	Duration         string   `json:"duration,omitempty"`
	Error            string   `json:"error,omitempty"`
	CleanupError     string   `json:"cleanupError,omitempty"`
}

func main() {
	specPath := flag.String("spec", "", "absolute command specification JSON")
	containmentSelfTest := flag.Bool("containment-selftest", false, "run the owned-process containment self-test")
	wrapperTimeoutFixture := flag.Bool("wrapper-timeout-fixture", false, "run the outer-wrapper timeout fixture")
	selfTestDir := flag.String("selftest-dir", "", "absolute self-test artifact directory")
	fixtureRole := flag.String("fixture-role", "", "internal fixture role")
	heartbeatPath := flag.String("heartbeat", "", "fixture heartbeat path")
	grandchildHeartbeatPath := flag.String("grandchild-heartbeat", "", "fake parent grandchild heartbeat path")
	readyPath := flag.String("fixture-ready", "", "fake parent readiness file")
	flag.Parse()
	if *fixtureRole != "" {
		if err := runFixture(*fixtureRole, *heartbeatPath, *grandchildHeartbeatPath, *readyPath); err != nil {
			os.Exit(1)
		}
		return
	}
	if *containmentSelfTest {
		if !filepath.IsAbs(*selfTestDir) {
			os.Exit(2)
		}
		if err := runContainmentSelfTest(*selfTestDir); err != nil {
			os.Exit(1)
		}
		return
	}
	if *wrapperTimeoutFixture {
		if !filepath.IsAbs(*selfTestDir) {
			os.Exit(2)
		}
		if err := runWrapperTimeoutFixture(*selfTestDir); err != nil {
			os.Exit(1)
		}
		return
	}
	if *specPath == "" {
		os.Exit(2)
	}
	if err := run(*specPath); err != nil {
		os.Exit(1)
	}
}

func runWrapperTimeoutFixture(root string) error {
	if err := os.MkdirAll(root, 0o700); err != nil {
		return err
	}
	if err := containCurrentProcess(); err != nil {
		return fmt.Errorf("self Job assignment failed: %w", err)
	}
	if err := ensureNoDeniedEnvironment(); err != nil {
		return err
	}
	if _, err := os.Stat(filepath.Join(root, "abort-before-start")); err == nil {
		return errors.New("wrapper requested abort before fixture child start")
	} else if !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("check wrapper abort marker: %w", err)
	}
	environment, err := selfTestEnvironment()
	if err != nil {
		return err
	}
	executable, err := os.Executable()
	if err != nil {
		return err
	}
	parentDir := filepath.Join(root, "fake-parent-owned")
	if err := os.MkdirAll(parentDir, 0o700); err != nil {
		return err
	}
	parentPulse := filepath.Join(root, "fake-parent-heartbeat.txt")
	grandchildPulse := filepath.Join(root, "fake-grandchild-heartbeat.txt")
	fixtureReady := filepath.Join(root, "fake-grandchild-ready.json")
	parent, err := ownedprocess.StartWithEnvironment(executable,
		[]string{"--fixture-role", "parent", "--heartbeat", parentPulse, "--grandchild-heartbeat", grandchildPulse, "--fixture-ready", fixtureReady}, parentDir, root, environment)
	if err != nil {
		return err
	}
	if err := waitForFile(fixtureReady, 8*time.Second); err != nil {
		_ = parent.Close()
		return err
	}
	if err := waitForHeartbeat(grandchildPulse, "", 5*time.Second); err != nil {
		_ = parent.Close()
		return err
	}
	if err := waitForHeartbeat(parentPulse, "", 5*time.Second); err != nil {
		_ = parent.Close()
		return err
	}
	return atomicJSON(filepath.Join(root, "wrapper-contained-ready.json"), map[string]any{
		"contained":               true,
		"processId":               os.Getpid(),
		"parentHeartbeatPath":     parentPulse,
		"grandchildHeartbeatPath": grandchildPulse,
		"parentStdoutPath":        parent.StdoutPath,
		"parentStderrPath":        parent.StderrPath,
	})
}

type selfTestEvidence struct {
	Status                                string            `json:"status"`
	StartedAt                             time.Time         `json:"startedAt"`
	FinishedAt                            time.Time         `json:"finishedAt"`
	Deadlines                             map[string]string `json:"deadlines"`
	SelfJobReady                          bool              `json:"selfJobReady"`
	OwnedStartMode                        string            `json:"ownedStartMode"`
	IndependentStarted                    bool              `json:"independentStarted"`
	ParentStarted                         bool              `json:"parentStarted"`
	GrandchildReady                       bool              `json:"grandchildReady"`
	GrandchildPipeObserved                bool              `json:"grandchildPipeObserved"`
	ParentOwnedCloseSucceeded             bool              `json:"parentOwnedCloseSucceeded"`
	ParentHandleSignaled                  bool              `json:"parentHandleSignaled"`
	ParentHeartbeatStopped                bool              `json:"parentHeartbeatStopped"`
	GrandchildHeartbeatStopped            bool              `json:"grandchildHeartbeatStopped"`
	IndependentAdvancedAfterParentClose   bool              `json:"independentAdvancedAfterParentClose"`
	IndependentOwnedCloseSucceeded        bool              `json:"independentOwnedCloseSucceeded"`
	IndependentHeartbeatStopped           bool              `json:"independentHeartbeatStopped"`
	SpecTimeoutStatus                     string            `json:"specTimeoutStatus,omitempty"`
	SpecTimeoutHelperExited               bool              `json:"specTimeoutHelperExited"`
	SpecTimeoutParentHeartbeatStopped     bool              `json:"specTimeoutParentHeartbeatStopped"`
	SpecTimeoutGrandchildHeartbeatStopped bool              `json:"specTimeoutGrandchildHeartbeatStopped"`
	SpecTimeoutIndependentAdvanced        bool              `json:"specTimeoutIndependentAdvanced"`
	SpecTimeoutRunnerStdoutPath           string            `json:"specTimeoutRunnerStdoutPath,omitempty"`
	SpecTimeoutRunnerStderrPath           string            `json:"specTimeoutRunnerStderrPath,omitempty"`
	SpecTimeoutRunnerStdoutSha256         string            `json:"specTimeoutRunnerStdoutSha256,omitempty"`
	SpecTimeoutRunnerStderrSha256         string            `json:"specTimeoutRunnerStderrSha256,omitempty"`
	SpecTimeoutResultPath                 string            `json:"specTimeoutResultPath,omitempty"`
	SpecTimeoutResultSha256               string            `json:"specTimeoutResultSha256,omitempty"`
	NormalSpecStatus                      string            `json:"normalSpecStatus,omitempty"`
	NormalSpecExited                      bool              `json:"normalSpecExited"`
	NormalSpecStdoutPath                  string            `json:"normalSpecStdoutPath,omitempty"`
	NormalSpecStderrPath                  string            `json:"normalSpecStderrPath,omitempty"`
	NormalSpecStdoutSha256                string            `json:"normalSpecStdoutSha256,omitempty"`
	NormalSpecStderrSha256                string            `json:"normalSpecStderrSha256,omitempty"`
	NormalSpecResultPath                  string            `json:"normalSpecResultPath,omitempty"`
	NormalSpecResultSha256                string            `json:"normalSpecResultSha256,omitempty"`
	ParentStdoutPath                      string            `json:"parentStdoutPath,omitempty"`
	ParentStderrPath                      string            `json:"parentStderrPath,omitempty"`
	ParentStdoutSha256                    string            `json:"parentStdoutSha256,omitempty"`
	ParentStderrSha256                    string            `json:"parentStderrSha256,omitempty"`
	Failure                               string            `json:"failure,omitempty"`
}

func runContainmentSelfTest(root string) error {
	if err := os.MkdirAll(root, 0o700); err != nil {
		return err
	}
	startedAt := time.Now().UTC()
	type workResult struct{ err error }
	done := make(chan workResult, 1)
	go func() { done <- workResult{err: runContainmentSelfTestWork(root)} }()
	timer := time.NewTimer(45 * time.Second)
	defer timer.Stop()
	select {
	case completed := <-done:
		workPath := filepath.Join(root, "containment-work-result.json")
		data, err := readBoundedFile(workPath, 1<<20)
		if err != nil {
			return fmt.Errorf("read fixed-deadline self-test evidence: %w", err)
		}
		var evidence any
		if err := json.Unmarshal(data, &evidence); err != nil {
			return fmt.Errorf("parse fixed-deadline self-test evidence: %w", err)
		}
		if err := writeResult(filepath.Join(root, "containment-result.json"), evidence); err != nil {
			return err
		}
		return completed.err
	case <-timer.C:
		evidence := map[string]any{
			"status":                     "failed-timeout",
			"startedAt":                  startedAt,
			"finishedAt":                 time.Now().UTC(),
			"overallTimeoutMilliseconds": 45000,
			"overallDeadline":            "fixed from startedAt; no deadline extension on heartbeat activity",
			"failure":                    "containment self-test exceeded its fixed overall deadline; process exit closes only its self Job and the externally-owned launcher Job",
		}
		writeErr := writeResult(filepath.Join(root, "containment-result.json"), evidence)
		if writeErr != nil {
			return writeErr
		}
		return errors.New("containment self-test exceeded fixed 45-second overall deadline")
	}
}

func runContainmentSelfTestWork(root string) (resultErr error) {
	if err := os.MkdirAll(root, 0o700); err != nil {
		return err
	}
	evidence := selfTestEvidence{
		Status:    "failed",
		StartedAt: time.Now().UTC(),
		Deadlines: map[string]string{
			"fixtureReadiness": "8s",
			"heartbeatAdvance": "5s",
			"ownedProcessExit": "5s",
			"heartbeatStop":    "3s",
			"overall":          "45s",
		},
		OwnedStartMode: "ownedprocess.StartWithEnvironment (CREATE_SUSPENDED -> AssignProcessToJobObject -> ResumeThread)",
	}
	defer func() {
		evidence.FinishedAt = time.Now().UTC()
		if resultErr != nil {
			evidence.Failure = resultErr.Error()
		} else {
			evidence.Status = "passed"
		}
		if err := writeResult(filepath.Join(root, "containment-work-result.json"), evidence); err != nil && resultErr == nil {
			resultErr = err
		}
	}()
	if err := containCurrentProcess(); err != nil {
		return fmt.Errorf("self Job assignment failed: %w", err)
	}
	evidence.SelfJobReady = true
	if err := ensureNoDeniedEnvironment(); err != nil {
		return err
	}

	independentDir := filepath.Join(root, "independent-owned")
	parentDir := filepath.Join(root, "fake-parent-owned")
	for _, dir := range []string{independentDir, parentDir} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
	}
	independentPulse := filepath.Join(root, "independent-heartbeat.txt")
	parentPulse := filepath.Join(root, "fake-parent-heartbeat.txt")
	grandchildPulse := filepath.Join(root, "fake-grandchild-heartbeat.txt")
	fixtureReady := filepath.Join(root, "fake-grandchild-ready.json")

	env, err := selfTestEnvironment()
	if err != nil {
		return err
	}
	executable, err := os.Executable()
	if err != nil {
		return err
	}
	independent, err := ownedprocess.StartWithEnvironment(executable,
		[]string{"--fixture-role", "heartbeat", "--heartbeat", independentPulse}, independentDir, root, env)
	if err != nil {
		return fmt.Errorf("start independently owned heartbeat: %w", err)
	}
	defer independent.Close()
	evidence.IndependentStarted = true
	if err := waitForHeartbeat(independentPulse, "", 5*time.Second); err != nil {
		return fmt.Errorf("independent heartbeat readiness: %w", err)
	}
	independentBefore, err := readHeartbeat(independentPulse)
	if err != nil {
		return err
	}
	if err := runSpecTimeoutSelfTest(root, executable, env, independentPulse, independentBefore, &evidence); err != nil {
		return err
	}
	if err := runSpecNormalSelfTest(root, executable, env, &evidence); err != nil {
		return err
	}

	parent, err := ownedprocess.StartWithEnvironment(executable,
		[]string{"--fixture-role", "parent", "--heartbeat", parentPulse, "--grandchild-heartbeat", grandchildPulse, "--fixture-ready", fixtureReady}, parentDir, root, env)
	if err != nil {
		return fmt.Errorf("start fake parent in Owned Job: %w", err)
	}
	evidence.ParentStarted = true
	evidence.ParentStdoutPath = parent.StdoutPath
	evidence.ParentStderrPath = parent.StderrPath
	parentClosed := false
	defer func() {
		if !parentClosed {
			_ = parent.Close()
		}
	}()
	if err := waitForFile(fixtureReady, 8*time.Second); err != nil {
		return fmt.Errorf("fake grandchild readiness: %w", err)
	}
	evidence.GrandchildReady = true
	if err := waitForHeartbeat(grandchildPulse, "", 5*time.Second); err != nil {
		return fmt.Errorf("fake grandchild heartbeat readiness: %w", err)
	}
	if err := waitForHeartbeat(parentPulse, "", 5*time.Second); err != nil {
		return fmt.Errorf("fake parent heartbeat readiness: %w", err)
	}
	evidence.GrandchildPipeObserved = true
	parentBefore, err := readHeartbeat(parentPulse)
	if err != nil {
		return err
	}
	grandchildBefore, err := readHeartbeat(grandchildPulse)
	if err != nil {
		return err
	}

	if err := parent.Close(); err != nil {
		return fmt.Errorf("close fake parent's Owned Job: %w", err)
	}
	parentClosed = true
	evidence.ParentOwnedCloseSucceeded = true
	exited, err := parent.Exited(5000)
	if err != nil {
		return fmt.Errorf("wait for fake parent held handle: %w", err)
	}
	evidence.ParentHandleSignaled = exited
	if !exited {
		return errors.New("fake parent process handle did not signal after Owned Job close")
	}
	if err := waitForHeartbeatStop(parentPulse, parentBefore, 3*time.Second); err != nil {
		return fmt.Errorf("fake parent heartbeat did not stop: %w", err)
	}
	evidence.ParentHeartbeatStopped = true
	if err := waitForHeartbeatStop(grandchildPulse, grandchildBefore, 3*time.Second); err != nil {
		return fmt.Errorf("fake grandchild heartbeat did not stop: %w", err)
	}
	evidence.GrandchildHeartbeatStopped = true
	if err := waitForHeartbeat(independentPulse, independentBefore, 5*time.Second); err != nil {
		return fmt.Errorf("independent Owned Job did not survive fake-parent close: %w", err)
	}
	evidence.IndependentAdvancedAfterParentClose = true
	if err := verifyOwnedLogs(parent.StdoutPath, parent.StderrPath); err != nil {
		return err
	}
	evidence.ParentStdoutSha256, err = hashBoundedFile(parent.StdoutPath, 1<<20)
	if err != nil {
		return err
	}
	evidence.ParentStderrSha256, err = hashBoundedFile(parent.StderrPath, 1<<20)
	if err != nil {
		return err
	}
	if err := independent.Close(); err != nil {
		return fmt.Errorf("close independent Owned Job: %w", err)
	}
	evidence.IndependentOwnedCloseSucceeded = true
	if err := waitForHeartbeatStop(independentPulse, independentBefore, 3*time.Second); err != nil {
		return fmt.Errorf("independent heartbeat did not stop after its Owned Job close: %w", err)
	}
	evidence.IndependentHeartbeatStopped = true
	return nil
}

func runSpecTimeoutSelfTest(root, executable string, environment []string, independentPulse, independentBefore string, evidence *selfTestEvidence) error {
	commandDir := filepath.Join(root, "spec-timeout-command")
	runnerDir := filepath.Join(root, "spec-timeout-runner")
	for _, dir := range []string{commandDir, runnerDir} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
	}
	parentPulse := filepath.Join(root, "spec-timeout-parent-heartbeat.txt")
	grandchildPulse := filepath.Join(root, "spec-timeout-grandchild-heartbeat.txt")
	fixtureReady := filepath.Join(root, "spec-timeout-grandchild-ready.json")
	readyPath := filepath.Join(commandDir, "contained-ready.json")
	abortPath := filepath.Join(commandDir, "abort-before-start")
	resultPath := filepath.Join(commandDir, "owned-result.json")
	specPath := filepath.Join(commandDir, "command.json")
	envMap := make(map[string]string, len(environment))
	allowlist := make([]string, 0, len(environment))
	for _, entry := range environment {
		key, value, ok := strings.Cut(entry, "=")
		if !ok {
			return errors.New("invalid self-test environment entry")
		}
		envMap[key] = value
		allowlist = append(allowlist, key)
	}
	sort.Strings(allowlist)
	spec := commandSpec{
		Executable:           executable,
		Arguments:            []string{"--fixture-role", "parent", "--heartbeat", parentPulse, "--grandchild-heartbeat", grandchildPulse, "--fixture-ready", fixtureReady},
		Environment:          envMap,
		EnvironmentAllowlist: allowlist,
		WorkingDirectory:     root,
		AllowedRunRoot:       root,
		RunDirectory:         commandDir,
		ReadyPath:            readyPath,
		AbortPath:            abortPath,
		ResultPath:           resultPath,
		TimeoutMilliseconds:  700,
	}
	if err := atomicJSON(specPath, spec); err != nil {
		return err
	}
	actualRunner, err := ownedprocess.StartWithEnvironment(executable, []string{"--spec", specPath}, runnerDir, root, environment)
	if err != nil {
		return fmt.Errorf("start actual --spec timeout runner in Owned Job: %w", err)
	}
	evidence.SpecTimeoutRunnerStdoutPath = actualRunner.StdoutPath
	evidence.SpecTimeoutRunnerStderrPath = actualRunner.StderrPath
	exited, waitErr := actualRunner.Exited(8000)
	if waitErr != nil {
		_ = actualRunner.Close()
		return fmt.Errorf("wait for --spec timeout runner: %w", waitErr)
	}
	if !exited {
		_ = actualRunner.Close()
		return errors.New("--spec timeout runner exceeded 8-second outer bound")
	}
	evidence.SpecTimeoutHelperExited = true
	code, err := actualRunner.ExitCode()
	if err != nil {
		_ = actualRunner.Close()
		return err
	}
	if code == 0 {
		_ = actualRunner.Close()
		return errors.New("--spec timeout runner returned success instead of timeout failure")
	}
	if err := actualRunner.Close(); err != nil {
		return fmt.Errorf("close --spec runner Owned Job: %w", err)
	}
	var result runResult
	resultBytes, err := readBoundedFile(resultPath, 1<<20)
	if err != nil {
		return fmt.Errorf("read --spec timeout result: %w", err)
	}
	if err := json.Unmarshal(resultBytes, &result); err != nil {
		return fmt.Errorf("parse --spec timeout result: %w", err)
	}
	evidence.SpecTimeoutStatus = result.Status
	evidence.SpecTimeoutResultPath = resultPath
	evidence.SpecTimeoutResultSha256, err = hashBoundedFile(resultPath, 1<<20)
	if err != nil {
		return err
	}
	if result.Status != "timed-out" || !result.TimedOut || !result.Contained || !result.CommandStarted {
		return fmt.Errorf("--spec timeout result is incomplete or not timed-out: status=%q contained=%t started=%t timedOut=%t", result.Status, result.Contained, result.CommandStarted, result.TimedOut)
	}
	if err := waitForFile(fixtureReady, 5*time.Second); err != nil {
		return fmt.Errorf("--spec timeout fake grandchild never became ready: %w", err)
	}
	evidence.GrandchildReady = true
	parentLast, err := readHeartbeat(parentPulse)
	if err != nil || parentLast == "" {
		return fmt.Errorf("--spec timeout fake parent heartbeat was not observed: %w", err)
	}
	grandchildLast, err := readHeartbeat(grandchildPulse)
	if err != nil || grandchildLast == "" {
		return fmt.Errorf("--spec timeout fake grandchild heartbeat was not observed: %w", err)
	}
	if err := waitForHeartbeatStop(parentPulse, parentLast, 3*time.Second); err != nil {
		return fmt.Errorf("--spec timed-out parent heartbeat did not stop: %w", err)
	}
	evidence.SpecTimeoutParentHeartbeatStopped = true
	if err := waitForHeartbeatStop(grandchildPulse, grandchildLast, 3*time.Second); err != nil {
		return fmt.Errorf("--spec timed-out grandchild heartbeat did not stop: %w", err)
	}
	evidence.SpecTimeoutGrandchildHeartbeatStopped = true
	evidence.SpecTimeoutIndependentAdvanced = waitForHeartbeat(independentPulse, independentBefore, 5*time.Second) == nil
	if !evidence.SpecTimeoutIndependentAdvanced {
		return errors.New("independent Owned Job did not advance after --spec timeout cleanup")
	}
	if err := verifyOwnedLogs(actualRunner.StdoutPath, actualRunner.StderrPath, result.StdoutPath, result.StderrPath); err != nil {
		return err
	}
	evidence.SpecTimeoutRunnerStdoutSha256, err = hashBoundedFile(actualRunner.StdoutPath, 1<<20)
	if err != nil {
		return err
	}
	evidence.SpecTimeoutRunnerStderrSha256, err = hashBoundedFile(actualRunner.StderrPath, 1<<20)
	if err != nil {
		return err
	}
	return nil
}

func runSpecNormalSelfTest(root, executable string, environment []string, evidence *selfTestEvidence) error {
	commandDir := filepath.Join(root, "spec-normal-command")
	runnerDir := filepath.Join(root, "spec-normal-runner")
	for _, dir := range []string{commandDir, runnerDir} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
	}
	marker := filepath.Join(root, "normal-command-completed.txt")
	readyPath := filepath.Join(commandDir, "contained-ready.json")
	abortPath := filepath.Join(commandDir, "abort-before-start")
	resultPath := filepath.Join(commandDir, "owned-result.json")
	specPath := filepath.Join(commandDir, "command.json")
	envMap := make(map[string]string, len(environment))
	allowlist := make([]string, 0, len(environment))
	for _, entry := range environment {
		key, value, ok := strings.Cut(entry, "=")
		if !ok {
			return errors.New("invalid normal self-test environment entry")
		}
		envMap[key] = value
		allowlist = append(allowlist, key)
	}
	sort.Strings(allowlist)
	spec := commandSpec{
		Executable:           executable,
		Arguments:            []string{"--fixture-role", "once", "--heartbeat", marker},
		Environment:          envMap,
		EnvironmentAllowlist: allowlist,
		WorkingDirectory:     root,
		AllowedRunRoot:       root,
		RunDirectory:         commandDir,
		ReadyPath:            readyPath,
		AbortPath:            abortPath,
		ResultPath:           resultPath,
		TimeoutMilliseconds:  5000,
	}
	if err := atomicJSON(specPath, spec); err != nil {
		return err
	}
	actualRunner, err := ownedprocess.StartWithEnvironment(executable, []string{"--spec", specPath}, runnerDir, root, environment)
	if err != nil {
		return fmt.Errorf("start normal --spec runner in Owned Job: %w", err)
	}
	evidence.NormalSpecStdoutPath = actualRunner.StdoutPath
	evidence.NormalSpecStderrPath = actualRunner.StderrPath
	exited, err := actualRunner.Exited(8000)
	if err != nil {
		_ = actualRunner.Close()
		return fmt.Errorf("wait normal --spec runner: %w", err)
	}
	if !exited {
		_ = actualRunner.Close()
		return errors.New("normal --spec runner exceeded 8-second bound")
	}
	evidence.NormalSpecExited = true
	code, err := actualRunner.ExitCode()
	if err != nil {
		_ = actualRunner.Close()
		return err
	}
	if code != 0 {
		_ = actualRunner.Close()
		return fmt.Errorf("normal --spec runner exited %d", code)
	}
	if err := actualRunner.Close(); err != nil {
		return fmt.Errorf("close normal --spec runner job: %w", err)
	}
	resultBytes, err := readBoundedFile(resultPath, 1<<20)
	if err != nil {
		return err
	}
	var result runResult
	if err := json.Unmarshal(resultBytes, &result); err != nil {
		return err
	}
	evidence.NormalSpecStatus = result.Status
	evidence.NormalSpecResultPath = resultPath
	evidence.NormalSpecResultSha256, err = hashBoundedFile(resultPath, 1<<20)
	if err != nil {
		return err
	}
	if result.Status != "completed" || result.TimedOut || !result.Contained || !result.CommandStarted {
		return fmt.Errorf("normal --spec evidence incomplete: status=%q contained=%t started=%t timedOut=%t", result.Status, result.Contained, result.CommandStarted, result.TimedOut)
	}
	if err := waitForFile(marker, 2*time.Second); err != nil {
		return fmt.Errorf("normal command completion marker missing: %w", err)
	}
	if err := verifyOwnedLogs(actualRunner.StdoutPath, actualRunner.StderrPath, result.StdoutPath, result.StderrPath); err != nil {
		return err
	}
	evidence.NormalSpecStdoutSha256, err = hashBoundedFile(actualRunner.StdoutPath, 1<<20)
	if err != nil {
		return err
	}
	evidence.NormalSpecStderrSha256, err = hashBoundedFile(actualRunner.StderrPath, 1<<20)
	if err != nil {
		return err
	}
	return nil
}

func runFixture(role, heartbeatPath, grandchildHeartbeatPath, readyPath string) error {
	switch role {
	case "heartbeat":
		return pulseForever(heartbeatPath)
	case "parent":
		if !filepath.IsAbs(grandchildHeartbeatPath) || !filepath.IsAbs(readyPath) {
			return errors.New("fake parent requires absolute fixture paths")
		}
		reader, writer, err := os.Pipe()
		if err != nil {
			return err
		}
		child, err := os.Executable()
		if err != nil {
			_ = reader.Close()
			_ = writer.Close()
			return err
		}
		cmd := exec.Command(child, "--fixture-role", "grandchild", "--heartbeat", grandchildHeartbeatPath)
		cmd.Stdout = writer
		cmd.Stderr = os.Stderr
		if err := cmd.Start(); err != nil {
			_ = reader.Close()
			_ = writer.Close()
			return err
		}
		_ = writer.Close()
		if err := atomicJSON(readyPath, map[string]any{"started": true, "at": time.Now().UTC()}); err != nil {
			_ = reader.Close()
			return err
		}
		pipeDone := make(chan struct{})
		go func() {
			_, _ = io.Copy(io.Discard, reader)
			_ = reader.Close()
			close(pipeDone)
		}()
		pulseDone := make(chan error, 1)
		go func() { pulseDone <- pulseForever(heartbeatPath) }()
		<-pipeDone
		if err := cmd.Wait(); err != nil {
			return err
		}
		return <-pulseDone
	case "grandchild":
		return pulseToStdoutForever(heartbeatPath)
	case "once":
		if !filepath.IsAbs(heartbeatPath) {
			return errors.New("completion marker path must be absolute")
		}
		return writeHeartbeat(heartbeatPath, 1)
	default:
		return fmt.Errorf("unknown fixture role %q", role)
	}
}

func pulseForever(path string) error {
	if !filepath.IsAbs(path) {
		return errors.New("heartbeat path must be absolute")
	}
	for sequence := uint64(1); ; sequence++ {
		if err := writeHeartbeat(path, sequence); err != nil {
			return err
		}
		time.Sleep(40 * time.Millisecond)
	}
}

func pulseToStdoutForever(path string) error {
	if !filepath.IsAbs(path) {
		return errors.New("heartbeat path must be absolute")
	}
	for sequence := uint64(1); ; sequence++ {
		if err := writeHeartbeat(path, sequence); err != nil {
			return err
		}
		if _, err := fmt.Fprintf(os.Stdout, "pipe-pulse-%d\n", sequence); err != nil {
			return err
		}
		time.Sleep(40 * time.Millisecond)
	}
}

func writeHeartbeat(path string, sequence uint64) error {
	return os.WriteFile(path, []byte(strconv.FormatUint(sequence, 10)), 0o600)
}

func readHeartbeat(path string) (string, error) {
	data, err := readBoundedFile(path, 64)
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(data)), nil
}

func waitForHeartbeat(path, previous string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		current, err := readHeartbeat(path)
		if err == nil && current != "" && current != previous {
			return nil
		}
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("heartbeat did not advance within %s", timeout)
}

func waitForHeartbeatStop(path, finalValue string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	stableSince := time.Now()
	for time.Now().Before(deadline) {
		current, err := readHeartbeat(path)
		if err != nil {
			return err
		}
		if current != finalValue {
			if current == "" {
				time.Sleep(20 * time.Millisecond)
				continue
			}
			finalValue = current
			stableSince = time.Now()
		} else if time.Since(stableSince) >= 250*time.Millisecond {
			return nil
		}
		time.Sleep(40 * time.Millisecond)
	}
	return fmt.Errorf("heartbeat did not remain stable for 250ms within %s", timeout)
}

func waitForFile(path string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(path); err == nil {
			return nil
		}
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("file %s did not appear within %s", filepath.Base(path), timeout)
}

func readBoundedFile(path string, maxBytes int64) ([]byte, error) {
	info, err := os.Stat(path)
	if err != nil {
		return nil, err
	}
	if info.Size() > maxBytes {
		return nil, fmt.Errorf("file %s exceeds %d-byte read limit", filepath.Base(path), maxBytes)
	}
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	data, err := io.ReadAll(io.LimitReader(file, maxBytes+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > maxBytes {
		return nil, fmt.Errorf("file %s grew beyond %d-byte read limit", filepath.Base(path), maxBytes)
	}
	return data, nil
}

func hashBoundedFile(path string, maxBytes int64) (string, error) {
	data, err := readBoundedFile(path, maxBytes)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:]), nil
}

func verifyOwnedLogs(paths ...string) error {
	for _, path := range paths {
		if _, err := readBoundedFile(path, 1<<20); err != nil {
			return fmt.Errorf("bounded owned log read failed: %w", err)
		}
	}
	return nil
}

func ensureNoDeniedEnvironment() error {
	for _, entry := range os.Environ() {
		key, _, _ := strings.Cut(entry, "=")
		if _, denied := deniedEnvironment[strings.ToUpper(key)]; denied {
			return fmt.Errorf("self-test refuses inherited credential/proxy variable %s", key)
		}
	}
	return nil
}

func selfTestEnvironment() ([]string, error) {
	values := make([]string, 0, len(os.Environ()))
	for _, entry := range os.Environ() {
		key, _, _ := strings.Cut(entry, "=")
		if _, denied := deniedEnvironment[strings.ToUpper(key)]; denied {
			return nil, fmt.Errorf("self-test environment contains denied key %s", key)
		}
		values = append(values, entry)
	}
	sort.Strings(values)
	return values, nil
}

func run(specPath string) error {
	result := runResult{Status: "blocked", Contained: false}
	spec, err := readSpec(specPath)
	if err != nil {
		return err
	}
	result.Executable = spec.Executable
	result.Arguments = append([]string(nil), spec.Arguments...)
	result.WorkingDirectory = spec.WorkingDirectory
	for key := range spec.Environment {
		result.EnvironmentKeys = append(result.EnvironmentKeys, key)
	}
	sort.Strings(result.EnvironmentKeys)

	if err := containCurrentProcess(); err != nil {
		result.Error = "helper self-job assignment failed: " + err.Error()
		_ = writeResult(spec.ResultPath, result)
		return err
	}
	result.Contained = true
	if err := writeReady(spec.ReadyPath); err != nil {
		result.Error = "could not publish containment readiness: " + err.Error()
		_ = writeResult(spec.ResultPath, result)
		return err
	}
	if _, err := os.Stat(spec.AbortPath); err == nil {
		result.Status = "aborted-before-command"
		_ = writeResult(spec.ResultPath, result)
		return errors.New("wrapper requested abort before command start")
	} else if !errors.Is(err, os.ErrNotExist) {
		result.Error = "could not check abort marker: " + err.Error()
		_ = writeResult(spec.ResultPath, result)
		return err
	}

	environment := make([]string, 0, len(spec.Environment))
	for key, value := range spec.Environment {
		environment = append(environment, key+"="+value)
	}
	sort.Strings(environment)
	child, err := ownedprocess.StartWithEnvironment(spec.Executable, spec.Arguments, spec.RunDirectory, spec.WorkingDirectory, environment)
	if err != nil {
		result.Status = "start-failed"
		result.Error = err.Error()
		_ = writeResult(spec.ResultPath, result)
		return err
	}
	result.CommandStarted = true
	result.StdoutPath = child.StdoutPath
	result.StderrPath = child.StderrPath
	started := time.Now()
	waitMS := uint32(spec.TimeoutMilliseconds)
	exited, waitErr := child.Exited(waitMS)
	result.Duration = time.Since(started).Round(time.Millisecond).String()
	if waitErr != nil {
		result.Status = "wait-failed"
		result.Error = waitErr.Error()
	} else if !exited {
		result.Status = "timed-out"
		result.TimedOut = true
	} else {
		code, codeErr := child.ExitCode()
		if codeErr != nil {
			result.Status = "exit-code-failed"
			result.Error = codeErr.Error()
		} else {
			result.ExitCode = &code
			if code == 0 {
				result.Status = "completed"
			} else {
				result.Status = "command-failed"
			}
		}
	}
	if closeErr := child.Close(); closeErr != nil {
		result.CleanupError = closeErr.Error()
		if result.Status == "completed" {
			result.Status = "cleanup-failed"
		}
	}
	if err := writeResult(spec.ResultPath, result); err != nil {
		return err
	}
	if result.Status != "completed" {
		return fmt.Errorf("owned command %s: %s", filepath.Base(spec.Executable), result.Status)
	}
	return nil
}

func readSpec(path string) (commandSpec, error) {
	var spec commandSpec
	if !filepath.IsAbs(path) {
		return spec, errors.New("spec path must be absolute")
	}
	file, err := os.Open(path)
	if err != nil {
		return spec, err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil || info.Size() > maxSpecBytes {
		return spec, errors.New("spec exceeds size limit")
	}
	decoder := json.NewDecoder(io.LimitReader(file, maxSpecBytes+1))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&spec); err != nil {
		return spec, err
	}
	if err := decoder.Decode(new(any)); !errors.Is(err, io.EOF) {
		return spec, errors.New("spec must contain exactly one JSON value")
	}
	if err := validateSpec(spec); err != nil {
		return spec, err
	}
	return spec, nil
}

func validateSpec(spec commandSpec) error {
	for name, value := range map[string]string{
		"executable": spec.Executable, "workingDirectory": spec.WorkingDirectory,
		"allowedRunRoot": spec.AllowedRunRoot, "runDirectory": spec.RunDirectory,
		"readyPath": spec.ReadyPath, "abortPath": spec.AbortPath, "resultPath": spec.ResultPath,
	} {
		if !filepath.IsAbs(value) {
			return fmt.Errorf("%s must be absolute", name)
		}
	}
	if spec.TimeoutMilliseconds <= 0 || spec.TimeoutMilliseconds > maxChildTimeoutMS {
		return errors.New("timeoutMilliseconds is outside the supported range")
	}
	if _, err := os.Stat(spec.Executable); err != nil {
		return errors.New("executable is unavailable")
	}
	info, err := os.Stat(spec.WorkingDirectory)
	if err != nil || !info.IsDir() {
		return errors.New("workingDirectory must exist and be a directory")
	}
	if info, err := os.Stat(spec.RunDirectory); err != nil || !info.IsDir() {
		return errors.New("runDirectory must exist and be a directory")
	}
	if info, err := os.Stat(spec.AllowedRunRoot); err != nil || !info.IsDir() {
		return errors.New("allowedRunRoot must exist and be a directory")
	}
	if !isWithin(spec.AllowedRunRoot, spec.RunDirectory) || !isWithin(spec.RunDirectory, spec.ReadyPath) || !isWithin(spec.RunDirectory, spec.AbortPath) || !isWithin(spec.RunDirectory, spec.ResultPath) {
		return errors.New("run and protocol files must remain inside the allowed run root")
	}
	allowed := make(map[string]struct{}, len(spec.EnvironmentAllowlist))
	for _, key := range spec.EnvironmentAllowlist {
		key = strings.ToUpper(key)
		if key == "" || strings.ContainsAny(key, "=\x00") {
			return errors.New("invalid environment allowlist key")
		}
		if _, denied := deniedEnvironment[key]; denied {
			return fmt.Errorf("credential or proxy key is always denied: %s", key)
		}
		allowed[key] = struct{}{}
	}
	for key, value := range spec.Environment {
		canonical := strings.ToUpper(key)
		if key == "" || strings.ContainsAny(key, "=\x00") || strings.ContainsRune(value, 0) {
			return errors.New("invalid environment entry")
		}
		if _, denied := deniedEnvironment[canonical]; denied {
			return fmt.Errorf("credential or proxy key is always denied: %s", canonical)
		}
		if _, ok := allowed[canonical]; !ok {
			return fmt.Errorf("environment key is outside the explicit allowlist: %s", canonical)
		}
	}
	return nil
}

func isWithin(root, path string) bool {
	rootAbs, err := filepath.Abs(root)
	if err != nil {
		return false
	}
	pathAbs, err := filepath.Abs(path)
	if err != nil {
		return false
	}
	rel, err := filepath.Rel(rootAbs, pathAbs)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) || filepath.IsAbs(rel) {
		return false
	}
	return true
}

func writeReady(path string) error {
	return atomicJSON(path, map[string]any{"contained": true, "processId": os.Getpid()})
}

func writeResult(path string, value any) error {
	return atomicJSON(path, value)
}

func atomicJSON(path string, value any) error {
	if !filepath.IsAbs(path) {
		return errors.New("output path must be absolute")
	}
	raw, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	tempFile, err := os.CreateTemp(filepath.Dir(path), ".finale2e-*.tmp")
	if err != nil {
		return err
	}
	temp := tempFile.Name()
	if err := tempFile.Chmod(0o600); err != nil {
		_ = tempFile.Close()
		_ = os.Remove(temp)
		return err
	}
	if _, err := tempFile.Write(raw); err != nil {
		_ = tempFile.Close()
		_ = os.Remove(temp)
		return err
	}
	if err := tempFile.Close(); err != nil {
		_ = os.Remove(temp)
		return err
	}
	if err := os.Rename(temp, path); err != nil {
		_ = os.Remove(temp)
		return err
	}
	return nil
}
