package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
)

type directRedrawReport struct {
	Passed        bool     `json:"passed"`
	Round         string   `json:"round"`
	RawFile       string   `json:"rawFile"`
	EscapedRaw    string   `json:"escapedRaw"`
	InitialOutput string   `json:"initialOutputBeforeRedraw"`
	InputTrace    string   `json:"inputTrace"`
	InputBytes    []string `json:"inputBytes"`
	SecuritySeen  bool     `json:"securityNotesSeen"`
	TerminalSeen  bool     `json:"terminalSetupSeen"`
	Initial81Rows []int    `json:"initial81Rows"`
	InitialHomes  []int    `json:"initialHomeTargets"`
	HomeTargets   []int    `json:"homeTargets"`
	Final42Rows   []int    `json:"final42Rows"`
	Final81Rows   []int    `json:"final81Rows"`
	ScreenLines   []string `json:"screenLines"`
	Error         string   `json:"error,omitempty"`
}

func runDirectRedrawDiagnostic(ctx context.Context, outputDir string) error {
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
	round, err := os.MkdirTemp(outputDir, "round-claude-redraw-*")
	if err != nil {
		return err
	}
	h := &harness{ctx: ctx, round: round,
		dataDir: filepath.Join(round, "data"), home: filepath.Join(round, "home"),
		workspace: filepath.Join(round, "workspace", "fintwind-claude-quota"),
		bin:       filepath.Join(round, "bin"), self: self,
		checks: map[string]bool{}, notes: map[string]string{},
	}
	if err := h.prepare(); err != nil {
		return err
	}
	root := filepath.Join(round, "direct-redraw")
	home := filepath.Join(root, "home")
	config := filepath.Join(home, ".claude")
	for _, dir := range []string{config, filepath.Join(home, "AppData", "Roaming"), filepath.Join(home, "AppData", "Local"), filepath.Join(root, "tmp")} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
	}
	seedBytes, _ := json.Marshal(seed{Status: "valid", Panel: "quota-redraw-probe"})
	if err := os.WriteFile(filepath.Join(config, "e2e-seed.json"), seedBytes, 0o600); err != nil {
		return err
	}
	rawPath := filepath.Join(round, "direct-redraw-output.bin")
	if err := os.WriteFile(rawPath, nil, 0o600); err != nil {
		return err
	}
	inputPath := filepath.Join(config, "e2e-panel-input.bin")
	env := ownedprocess.BuildEnv(root, map[string]string{
		"PATH": h.bin, "HOME": home, "USERPROFILE": home,
		"APPDATA": filepath.Join(home, "AppData", "Roaming"), "LOCALAPPDATA": filepath.Join(home, "AppData", "Local"),
		"CLAUDE_CONFIG_DIR": config, "TEMP": filepath.Join(root, "tmp"), "TMP": filepath.Join(root, "tmp"),
		"DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1", "DISABLE_AUTOUPDATER": "1", "TERM": "xterm-256color",
	})
	pty, err := ownedprocess.StartPTY(filepath.Join(h.bin, "claude.exe"), fixedPanelArgs(uuid.NewString()), root, h.workspace, env)
	if err != nil {
		return fmt.Errorf("StartPTY: %w", err)
	}
	defer pty.Close()
	var raw []byte
	appendOutput := func(chunk []byte) error {
		raw = append(raw, chunk...)
		return os.WriteFile(rawPath, raw, 0o600)
	}
	timeout := time.NewTimer(20 * time.Second)
	defer timeout.Stop()
	securitySent, terminalSent, styleSent, browserSent := false, false, false, false
	trustSent, usageSent, redrawReleased := false, false, false
	var initialRaw []byte
	const fullInput = "\r\x1b\r\x1b\x1b[B\r/usage\r"
	for {
		select {
		case chunk, ok := <-pty.Output:
			if !ok {
				return errors.New("ConPTY output closed before redraw capture")
			}
			if err := appendOutput(chunk); err != nil {
				return err
			}
			if !securitySent && bytes.Contains(raw, []byte("Press Enter to continue")) {
				securitySent = true
				if err := pty.Write([]byte{'\r'}); err != nil {
					return err
				}
			}
			if securitySent && !terminalSent && bytes.Contains(raw, []byte("ESC to skip")) {
				terminalSent = true
				if err := pty.Write([]byte{0x1b}); err != nil {
					return err
				}
			}
			if terminalSent && !styleSent && bytes.Contains(raw, []byte("Press Enter")) && bytes.Contains(raw, []byte("Choose the text style")) {
				styleSent = true
				if err := pty.Write([]byte{'\r'}); err != nil {
					return err
				}
			}
			if styleSent && !browserSent && bytes.Contains(raw, []byte("Press Escape")) {
				browserSent = true
				if err := pty.Write([]byte{0x1b}); err != nil {
					return err
				}
			}
			if browserSent && !trustSent && bytes.Contains(raw, []byte("Yes, I trust this folder")) {
				trustSent = true
				if err := pty.Write([]byte{0x1b, '[', 'B', '\r'}); err != nil {
					return err
				}
			}
			if trustSent && !usageSent && bytes.Contains(raw, []byte("Welcome")) {
				usageSent = true
				if err := pty.Write([]byte("/usage\r")); err != nil {
					return err
				}
			}
			if usageSent && !redrawReleased && bytes.Contains(raw, []byte("Current session: 81%")) && bytes.Contains(raw, []byte("Using your overages")) {
				initialRaw = append([]byte(nil), raw...)
				continuePath := filepath.Join(config, "e2e-redraw-continue.marker")
				if err := os.WriteFile(continuePath, []byte("captured-initial-screen"), 0o600); err != nil {
					return fmt.Errorf("release redraw probe: %w", err)
				}
				redrawReleased = true
			}
			if redrawReleased && bytes.Contains(raw, []byte("Current session: 42%")) {
				// Give ConPTY time to deliver all output from the fake's completed draw.
				select {
				case chunk, ok := <-pty.Output:
					if ok {
						if err := appendOutput(chunk); err != nil {
							return err
						}
					}
				case <-time.After(150 * time.Millisecond):
				}
				goto captured
			}
		case <-timeout.C:
			return fmt.Errorf("20s timeout waiting for fake redraw; onboarding=%v/%v/%v/%v/%v usage=%v", securitySent, terminalSent, styleSent, browserSent, trustSent, usageSent)
		case <-ctx.Done():
			return ctx.Err()
		}
	}

captured:
	if err := pty.Close(); err != nil {
		return err
	}
	input, err := os.ReadFile(inputPath)
	if err != nil {
		return fmt.Errorf("read input trace: %w", err)
	}
	initialLines, initialHomes, rows81, _ := replayVTLines(initialRaw)
	lines, homes, finalRows81, rows42 := replayVTLines(raw)
	report := directRedrawReport{
		Passed: string(input) == fullInput && securitySent && terminalSent && styleSent && browserSent && trustSent && usageSent && redrawReleased && len(rows81) == 1 && len(rows42) == 1 && rows81[0] == 1 && rows42[0] == 1 && len(initialHomes) > 0 && len(homes) > 0 && len(finalRows81) == 0,
		Round:  round, RawFile: rawPath, EscapedRaw: escapeDiagnosticBytes(raw), InputTrace: inputPath,
		InitialOutput: escapeDiagnosticBytes(initialRaw),
		InputBytes:    hexBytes(input), SecuritySeen: securitySent, TerminalSeen: terminalSent,
		Initial81Rows: rows81, InitialHomes: initialHomes, HomeTargets: homes, Final42Rows: rows42, Final81Rows: finalRows81, ScreenLines: append(initialLines, lines...),
	}
	reportPath := filepath.Join(round, "direct-redraw-evidence.json")
	body, err := json.MarshalIndent(report, "", "  ")
	if err != nil {
		return err
	}
	if err := os.WriteFile(reportPath, body, 0o600); err != nil {
		return err
	}
	fmt.Println("[DIRECT-REDRAW-REPORT]", reportPath)
	fmt.Println("[DIRECT-REDRAW-RAW]", rawPath)
	fmt.Printf("[DIRECT-REDRAW-PHYSICAL] input=%q initial-81%% rows=%v initial-home=%v final-81%% rows=%v redraw-home=%v final-42%% rows=%v\n", string(input), rows81, initialHomes, finalRows81, homes, rows42)
	if !report.Passed {
		return fmt.Errorf("direct physical-row diagnostic did not capture expected full onboarding and redraw")
	}
	return nil
}

// replayVTLines tracks printable output and the cursor for the CSI controls
// used by the fake. Rows are reported one-based, as in terminal coordinates.
func replayVTLines(raw []byte) ([]string, []int, []int, []int) {
	grid := make([][]byte, 50)
	for i := range grid {
		grid[i] = make([]byte, 200)
	}
	row, col := 0, 0
	var homes []int
	for i := 0; i < len(raw); {
		b := raw[i]
		if b == 0x1b && i+1 < len(raw) && raw[i+1] == '[' {
			j := i + 2
			for j < len(raw) && (raw[j] < 0x40 || raw[j] > 0x7e) {
				j++
			}
			if j >= len(raw) {
				break
			}
			params := string(raw[i+2 : j])
			final := raw[j]
			if final == 'H' || final == 'f' {
				parts := strings.Split(params, ";")
				r, c := 1, 1
				if len(parts) > 0 && parts[0] != "" {
					_, _ = fmt.Sscanf(parts[0], "%d", &r)
				}
				if len(parts) > 1 && parts[1] != "" {
					_, _ = fmt.Sscanf(parts[1], "%d", &c)
				}
				row, col = max(0, r-1), max(0, c-1)
				if params == "" || params == "1;1" {
					homes = append(homes, row+1)
				}
			} else if final == 'J' && params == "2" {
				for r := range grid {
					clear(grid[r])
				}
			} else if final == 'K' {
				if row >= 0 && row < len(grid) {
					clear(grid[row][min(col, len(grid[row])):])
				}
			}
			i = j + 1
			continue
		}
		switch b {
		case '\r':
			col = 0
		case '\n':
			row = min(row+1, len(grid)-1)
		case '\t':
			col = min((col/8+1)*8, len(grid[0])-1)
		default:
			if b >= 0x20 && b < 0x7f && row < len(grid) && col < len(grid[row]) {
				grid[row][col] = b
				col++
			}
		}
		i++
	}
	lines := make([]string, len(grid))
	var rows81, rows42 []int
	for i, line := range grid {
		text := strings.TrimRight(string(line), "\x00 ")
		lines[i] = text
		if strings.Contains(text, "Current session: 81%") {
			rows81 = append(rows81, i+1)
		}
		if strings.Contains(text, "Current session: 42%") {
			rows42 = append(rows42, i+1)
		}
	}
	return lines, homes, rows81, rows42
}
