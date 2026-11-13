package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
)

type directOnboardingEvidence struct {
	Passed          bool                `json:"passed"`
	Variant         string              `json:"variant"`
	Round           string              `json:"round"`
	RawOutput       string              `json:"rawOutput"`
	EscapedOutput   string              `json:"escapedOutput"`
	SecuritySeen    bool                `json:"securityNotesSeen"`
	TerminalSeen    bool                `json:"terminalSetupSeen"`
	SentBytes       []string            `json:"sentBytes"`
	ReceivedBytes   []string            `json:"receivedBytes"`
	ConsoleModes    consoleModeEvidence `json:"consoleModes"`
	ConsoleModeFile string              `json:"consoleModeFile"`
	InputTraceFile  string              `json:"inputTraceFile"`
	Error           string              `json:"error,omitempty"`
}

func runDirectOnboarding(ctx context.Context, outputDir string) error {
	if outputDir == "" {
		outputDir = filepath.Join("..", "target", "go-backend-claudequota", "reports")
	}
	outputDir, err := filepath.Abs(outputDir)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(outputDir, 0o700); err != nil {
		return err
	}
	self, err := os.Executable()
	if err != nil {
		return err
	}
	round, err := os.MkdirTemp(outputDir, "round-claude-onboarding-*")
	if err != nil {
		return err
	}
	h := &harness{
		ctx: ctx, round: round,
		dataDir: filepath.Join(round, "data"), home: filepath.Join(round, "home"),
		workspace: filepath.Join(round, "workspace", "fintwind-claude-quota"),
		bin:       filepath.Join(round, "bin"), self: self,
		checks: map[string]bool{}, notes: map[string]string{},
	}
	if err := h.prepare(); err != nil {
		return err
	}
	baseline := directOnboardingEvidence{
		Round: round, Variant: "existing console mode",
		RawOutput:       filepath.Join(round, "direct-onboarding-output.bin"),
		ConsoleModeFile: filepath.Join(round, "direct-onboarding", "home", ".claude", "e2e-console-modes.json"),
		InputTraceFile:  filepath.Join(round, "direct-onboarding", "home", ".claude", "e2e-panel-input.bin"),
		SentBytes:       []string{}, ReceivedBytes: []string{},
	}
	baselineErr := h.directOnboardingProbeVariant(&baseline, "direct-onboarding", false)
	baseline.Passed = baselineErr == nil
	if baselineErr != nil {
		baseline.Error = baselineErr.Error()
	}
	baselinePath := filepath.Join(round, "direct-onboarding-evidence.json")
	if err := saveOnboardingEvidence(baselinePath, baseline); err != nil {
		return errors.Join(baselineErr, err)
	}
	printOnboardingEvidence(baselinePath, baseline)

	// The baseline raw bytes and received input are saved before changing only
	// the fake's console input mode for this second discriminating variant.
	rawMode := directOnboardingEvidence{
		Round: round, Variant: "raw console input mode",
		RawOutput:       filepath.Join(round, "direct-onboarding-raw-output.bin"),
		ConsoleModeFile: filepath.Join(round, "direct-onboarding-raw", "home", ".claude", "e2e-console-modes.json"),
		InputTraceFile:  filepath.Join(round, "direct-onboarding-raw", "home", ".claude", "e2e-panel-input.bin"),
		SentBytes:       []string{}, ReceivedBytes: []string{},
	}
	rawErr := h.directOnboardingProbeVariant(&rawMode, "direct-onboarding-raw", true)
	rawMode.Passed = rawErr == nil
	if rawErr != nil {
		rawMode.Error = rawErr.Error()
	}
	rawModePath := filepath.Join(round, "direct-onboarding-raw-evidence.json")
	if err := saveOnboardingEvidence(rawModePath, rawMode); err != nil {
		return errors.Join(baselineErr, rawErr, err)
	}
	printOnboardingEvidence(rawModePath, rawMode)
	return errors.Join(baselineErr, rawErr)
}

func saveOnboardingEvidence(path string, evidence directOnboardingEvidence) error {
	body, err := json.MarshalIndent(evidence, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, body, 0o600)
}

func printOnboardingEvidence(path string, evidence directOnboardingEvidence) {
	fmt.Println("[DIRECT-ONBOARDING-EVIDENCE]", path)
	fmt.Println("[DIRECT-ONBOARDING-RAW]", evidence.RawOutput)
	fmt.Println("[DIRECT-ONBOARDING-ESCAPED]", evidence.EscapedOutput)
	fmt.Println("[DIRECT-ONBOARDING-INPUT] variant=", evidence.Variant, "sent=", evidence.SentBytes, "received=", evidence.ReceivedBytes)
	fmt.Printf("[DIRECT-ONBOARDING-CONSOLE] in=%v out=%v stdin-before=0x%04x stdin-after=0x%04x stdout=0x%04x line=%v->%v echo=%v->%v processed=%v->%v vt-input=%v->%v set-error=%q\n",
		evidence.ConsoleModes.ConsoleIn, evidence.ConsoleModes.ConsoleOut,
		evidence.ConsoleModes.StdinBefore, evidence.ConsoleModes.StdinAfter, evidence.ConsoleModes.StdoutMode,
		evidence.ConsoleModes.LineInputBefore, evidence.ConsoleModes.LineInputAfter,
		evidence.ConsoleModes.EchoInputBefore, evidence.ConsoleModes.EchoInputAfter,
		evidence.ConsoleModes.ProcessedBefore, evidence.ConsoleModes.ProcessedAfter,
		evidence.ConsoleModes.VirtualInputBefore, evidence.ConsoleModes.VirtualInputAfter,
		evidence.ConsoleModes.StdinAfterError)
}

func (h *harness) directOnboardingProbeVariant(evidence *directOnboardingEvidence, folder string, rawInput bool) error {
	root := filepath.Join(h.round, folder)
	home := filepath.Join(root, "home")
	config := filepath.Join(home, ".claude")
	for _, dir := range []string{config, filepath.Join(home, "AppData", "Roaming"), filepath.Join(home, "AppData", "Local"), filepath.Join(root, "tmp")} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
	}
	panel := "quota"
	if rawInput {
		panel = "raw-input-probe"
	}
	seedBytes, _ := json.Marshal(seed{Status: "valid", Panel: panel})
	if err := os.WriteFile(filepath.Join(config, "e2e-seed.json"), seedBytes, 0o600); err != nil {
		return err
	}
	if err := os.WriteFile(evidence.RawOutput, nil, 0o600); err != nil {
		return err
	}
	args := fixedPanelArgs(uuid.NewString())
	env := ownedprocess.BuildEnv(root, map[string]string{
		"PATH": h.bin, "HOME": home, "USERPROFILE": home,
		"APPDATA": filepath.Join(home, "AppData", "Roaming"), "LOCALAPPDATA": filepath.Join(home, "AppData", "Local"),
		"CLAUDE_CONFIG_DIR": config, "TEMP": filepath.Join(root, "tmp"), "TMP": filepath.Join(root, "tmp"),
		"DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1", "DISABLE_AUTOUPDATER": "1", "TERM": "xterm-256color",
	})
	pty, err := ownedprocess.StartPTY(filepath.Join(h.bin, "claude.exe"), args, root, h.workspace, env)
	if err != nil {
		return fmt.Errorf("StartPTY: %w", err)
	}
	defer pty.Close()
	timeout := time.NewTimer(15 * time.Second)
	defer timeout.Stop()
	tick := time.NewTicker(10 * time.Millisecond)
	defer tick.Stop()
	var raw []byte
	defer func() {
		evidence.EscapedOutput = escapeDiagnosticBytes(raw)
		if modeRaw, readErr := os.ReadFile(evidence.ConsoleModeFile); readErr == nil {
			_ = json.Unmarshal(modeRaw, &evidence.ConsoleModes)
		}
		if input, readErr := os.ReadFile(evidence.InputTraceFile); readErr == nil {
			evidence.ReceivedBytes = hexBytes(input)
		}
	}()
	sentCR, sentESC, receivedESC := false, false, false
	for !receivedESC {
		select {
		case chunk, ok := <-pty.Output:
			if !ok {
				return errors.New("ConPTY output closed before fake consumed both diagnostic bytes")
			}
			raw = append(raw, chunk...)
			if err := os.WriteFile(evidence.RawOutput, raw, 0o600); err != nil {
				return err
			}
			if !sentCR && bytes.Contains(raw, []byte("Security notes")) && bytes.Contains(raw, []byte("Press Enter to continue")) {
				evidence.SecuritySeen = true
				if err := pty.Write([]byte{'\r'}); err != nil {
					return fmt.Errorf("write security-note CR: %w", err)
				}
				evidence.SentBytes = append(evidence.SentBytes, "0x0d")
				sentCR = true
			}
			if sentCR && !sentESC && bytes.Contains(raw, []byte("Terminal setup")) && bytes.Contains(raw, []byte("ESC to skip")) {
				evidence.TerminalSeen = true
				if err := pty.Write([]byte{0x1b}); err != nil {
					return fmt.Errorf("write terminal-setup ESC: %w", err)
				}
				evidence.SentBytes = append(evidence.SentBytes, "0x1b")
				sentESC = true
			}
		case <-tick.C:
			if input, readErr := os.ReadFile(evidence.InputTraceFile); readErr == nil {
				evidence.ReceivedBytes = hexBytes(input)
				if bytes.Equal(input, []byte{'\r', 0x1b}) {
					receivedESC = true
				}
			}
			exited, waitErr := pty.Owned.Exited(0)
			if waitErr != nil {
				return waitErr
			}
			if exited {
				return errors.New("fake ConPTY process exited before recording CR and ESC")
			}
		case <-timeout.C:
			return errors.New("15s timeout waiting for two-stage fake ConPTY input")
		}
	}
	if err := pty.Close(); err != nil {
		return fmt.Errorf("close diagnostic PTY: %w", err)
	}
	evidence.EscapedOutput = escapeDiagnosticBytes(raw)
	modeRaw, err := os.ReadFile(evidence.ConsoleModeFile)
	if err != nil {
		return fmt.Errorf("read console mode evidence: %w", err)
	}
	if err := json.Unmarshal(modeRaw, &evidence.ConsoleModes); err != nil {
		return fmt.Errorf("decode console mode evidence: %w", err)
	}
	input, err := os.ReadFile(evidence.InputTraceFile)
	if err != nil {
		return fmt.Errorf("read fake input trace: %w", err)
	}
	evidence.ReceivedBytes = hexBytes(input)
	if !evidence.SecuritySeen || !evidence.TerminalSeen || !bytes.Equal(input, []byte{'\r', 0x1b}) {
		return fmt.Errorf("two-stage input mismatch: security=%v terminal=%v sent=%v received=%v", evidence.SecuritySeen, evidence.TerminalSeen, evidence.SentBytes, evidence.ReceivedBytes)
	}
	if !evidence.ConsoleModes.ConsoleIn || !evidence.ConsoleModes.ConsoleOut {
		return errors.New("fake stdin/stdout did not pass GetConsoleMode")
	}
	return nil
}

func hexBytes(value []byte) []string {
	result := make([]string, len(value))
	for i, b := range value {
		result[i] = fmt.Sprintf("0x%02x", b)
	}
	return result
}

func escapeDiagnosticBytes(value []byte) string {
	var out bytes.Buffer
	for _, b := range value {
		switch {
		case b == '\x1b':
			out.WriteString(`\x1b`)
		case b == '\r':
			out.WriteString(`\r`)
		case b == '\n':
			out.WriteString(`\n`)
		case b == '\t':
			out.WriteString(`\t`)
		case b >= 0x20 && b <= 0x7e:
			out.WriteByte(b)
		default:
			fmt.Fprintf(&out, `\x%02x`, b)
		}
	}
	return out.String()
}
