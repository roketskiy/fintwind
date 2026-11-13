//go:build windows

package backendquota

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendprivate"
	"fintwind-go/internal/ownedprocess"
	"github.com/google/uuid"
)

func (l *Local) claudeFingerprint() ([32]byte, error) {
	root, err := openQuotaHome(l.home)
	if err != nil {
		return [32]byte{}, err
	}
	defer root.Close()
	file, err := root.Open(filepath.Join(".claude", ".credentials.json"))
	if os.IsNotExist(err) {
		return sha256.Sum256(nil), nil
	}
	if err != nil {
		return [32]byte{}, err
	}
	defer file.Close()
	raw, err := io.ReadAll(io.LimitReader(file, 1<<20+1))
	if err != nil || len(raw) > 1<<20 {
		return [32]byte{}, errors.New("invalid Claude credential fingerprint")
	}
	return sha256.Sum256(raw), nil
}
func (l *Local) claudeEnvironment() []string {
	return ownedprocess.BuildEnv(filepath.Dir(l.home), map[string]string{"HOME": l.home, "USERPROFILE": l.home, "APPDATA": filepath.Join(l.home, "AppData", "Roaming"), "LOCALAPPDATA": filepath.Join(l.home, "AppData", "Local"), "CLAUDE_CONFIG_DIR": filepath.Join(l.home, ".claude"), "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1", "DISABLE_AUTOUPDATER": "1", "TERM": "xterm-256color"})
}

func (l *Local) fetchClaude(ctx context.Context, force bool) Quota {
	l.claudeMu.Lock()
	defer l.claudeMu.Unlock()
	if ctx.Err() != nil {
		return empty("unavailable")
	}
	binary, err := exec.LookPath("claude")
	if err != nil {
		return empty("no_credential")
	}
	binary, err = filepath.Abs(binary)
	if err != nil {
		return empty("unavailable")
	}
	fingerprint, err := l.claudeFingerprint()
	if err != nil {
		return empty("unavailable")
	}
	now := time.Now().Unix()
	floor := l.claudeAuto
	if force {
		floor = l.claudeManual
	}
	if fingerprint == l.claudeIdentity && now < floor && l.claudeLast != nil {
		return *l.claudeLast
	}
	root := filepath.Dir(l.home)
	cwd := filepath.Join(root, "workspace", "fintwind-claude-quota")
	profile, err := os.OpenRoot(root)
	if err != nil {
		return empty("unavailable")
	}
	defer profile.Close()
	for _, path := range []string{l.home, filepath.Join(l.home, ".claude"), cwd, filepath.Join(root, "tmp"), filepath.Join(l.home, "AppData", "Roaming"), filepath.Join(l.home, "AppData", "Local")} {
		relative, e := filepath.Rel(root, path)
		if e != nil || !filepath.IsLocal(relative) {
			return empty("unavailable")
		}
		if err := profile.MkdirAll(relative, 0o700); err != nil {
			return empty("unavailable")
		}
	}
	canonicalRoot, e := filepath.EvalSymlinks(root)
	if e != nil {
		return empty("unavailable")
	}
	for _, path := range []string{l.home, filepath.Join(l.home, ".claude"), cwd, filepath.Join(root, "tmp"), filepath.Join(l.home, "AppData", "Roaming"), filepath.Join(l.home, "AppData", "Local")} {
		canonical, e := filepath.EvalSymlinks(path)
		if e != nil {
			return empty("unavailable")
		}
		relative, e := filepath.Rel(canonicalRoot, canonical)
		if e != nil || !filepath.IsLocal(relative) {
			return empty("unavailable")
		}
	}
	if err := backendprivate.Directory(filepath.Join(l.home, ".claude")); err != nil {
		return empty("unavailable")
	}
	statusCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	raw, err := l.claudeStatus(statusCtx, binary, cwd)
	cancel()
	if err != nil {
		return empty("unavailable")
	}
	var login struct {
		LoggedIn     *bool  `json:"loggedIn"`
		Email        string `json:"email"`
		Subscription string `json:"subscriptionType"`
	}
	if json.Unmarshal(raw, &login) != nil || login.LoggedIn == nil {
		return empty("unavailable")
	}
	if !*login.LoggedIn {
		l.claudeLast = nil
		l.claudeGood = nil
		return empty("no_credential")
	}
	identity := sha256.Sum256(append(append([]byte(login.Email+"\x00"+login.Subscription+"\x00"), fingerprint[:]...), []byte(binary)...))
	// Credential rotation invalidates any previously cached account's reading.
	if fingerprint != l.claudeIdentity {
		l.claudeLast = nil
		l.claudeGood = nil
		l.claudeIdentity = fingerprint
	}
	if identity != l.claudeAccount {
		l.claudeLast = nil
		l.claudeGood = nil
		l.claudeAccount = identity
	}
	reading := empty("unavailable")
	query, cancel := context.WithTimeout(ctx, 65*time.Second)
	reading = l.claudePanel(query, binary, cwd)
	cancel()
	verified, err := l.claudeFingerprint()
	if err != nil || verified != fingerprint {
		l.claudeLast = nil
		l.claudeGood = nil
		return empty("unavailable")
	}
	verifyCtx, verifyCancel := context.WithTimeout(ctx, 5*time.Second)
	verifiedRaw, verifyErr := l.claudeStatus(verifyCtx, binary, cwd)
	verifyCancel()
	var confirmed struct {
		LoggedIn     *bool  `json:"loggedIn"`
		Email        string `json:"email"`
		Subscription string `json:"subscriptionType"`
	}
	if verifyErr != nil || json.Unmarshal(verifiedRaw, &confirmed) != nil || confirmed.LoggedIn == nil || !*confirmed.LoggedIn || confirmed.Email != login.Email || confirmed.Subscription != login.Subscription {
		l.claudeLast = nil
		l.claudeGood = nil
		return empty("unavailable")
	}
	reading.Service = ptr("Claude Code")
	label := login.Email
	if label == "" {
		label = "Claude"
	}
	reading.Account = &label
	if login.Subscription != "" {
		reading.Plan = &login.Subscription
	}
	now = time.Now().Unix()
	l.claudeAuto = now + 60
	l.claudeManual = now + 15
	if reading.Status == "ready" {
		reading.MeasuredAt = &now
		copy := reading
		l.claudeGood = &copy
	} else if reading.Status == "unavailable" || reading.Status == "rate_limited" {
		if l.claudeGood != nil && l.claudeGood.MeasuredAt != nil && now-*l.claudeGood.MeasuredAt < 86400 {
			status := reading.Status
			reading = *l.claudeGood
			reading.Status = status
		}
	} else {
		l.claudeGood = nil
	}
	if reading.Status != "ready" {
		reading.RetryAt = &l.claudeAuto
	}
	copy := reading
	l.claudeLast = &copy
	return reading
}

func (l *Local) claudeStatus(ctx context.Context, binary, cwd string) ([]byte, error) {
	out, writer, err := os.Pipe()
	if err != nil {
		return nil, err
	}
	defer out.Close()
	defer writer.Close()
	stderr, err := os.OpenFile(os.DevNull, os.O_WRONLY, 0)
	if err != nil {
		return nil, err
	}
	defer stderr.Close()
	child, err := ownedprocess.StartWithOutput(binary, []string{"auth", "status", "--json"}, filepath.Dir(l.home), cwd, l.claudeEnvironment(), writer, stderr)
	writer.Close()
	if err != nil {
		return nil, err
	}
	defer child.Close()
	type result struct {
		raw []byte
		err error
	}
	done := make(chan result, 1)
	go func() { raw, e := io.ReadAll(io.LimitReader(out, 1<<20+1)); done <- result{raw, e} }()
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	exited := false
	var exitCode uint32
	for {
		select {
		case <-ctx.Done():
			child.Close()
			out.Close()
			<-done
			return nil, ctx.Err()
		case value := <-done:
			if value.err != nil || len(value.raw) > 1<<20 {
				return nil, errors.New("Claude login output unavailable")
			}
			if !exited {
				for {
					if ctx.Err() != nil {
						return nil, ctx.Err()
					}
					ended, e := child.Exited(25)
					if e != nil {
						return nil, e
					}
					if ended {
						break
					}
				}
				var e error
				exitCode, e = child.ExitCode()
				if e != nil {
					return nil, e
				}
			}
			_ = exitCode // auth status may exit nonzero with valid loggedIn:false JSON.
			return value.raw, nil
		case <-ticker.C:
			if !exited {
				ended, e := child.Exited(0)
				if e != nil {
					return nil, e
				}
				if ended {
					exited = true
					exitCode, e = child.ExitCode()
					if e != nil {
						return nil, errors.New("Claude login command failed")
					}
					child.Close()
				}
			}
		}
	}
}

func (l *Local) claudePanel(ctx context.Context, binary, cwd string) Quota {
	session := uuid.NewString()
	defer l.dropClaudeSession(cwd, session)
	args := []string{"--setting-sources", "", "--settings", `{"disableDeepLinkRegistration":"disable"}`, "--strict-mcp-config", "--tools", "", "--permission-mode", "default", "--allowedTools", "", "--no-chrome", "--session-id", session}
	pty, err := ownedprocess.StartPTY(binary, args, filepath.Dir(l.home), cwd, l.claudeEnvironment())
	if err != nil {
		return empty("unavailable")
	}
	defer pty.Close()
	screen := quotaScreen{}
	started := time.Now()
	lastChange := started
	lastText := ""
	panelStart := time.Time{}
	sent := false
	retried := false
	onboard := map[string]bool{}
	trusts := 0
	tick := time.NewTicker(100 * time.Millisecond)
	defer tick.Stop()
	for {
		select {
		case <-ctx.Done():
			return empty("unavailable")
		case chunk, ok := <-pty.Output:
			if !ok {
				return empty("unavailable")
			}
			if reply := screen.feed(chunk); reply != "" {
				if pty.Write([]byte(reply)) != nil {
					return empty("unavailable")
				}
			}
			lastChange = time.Now()
		case <-tick.C:
			if ended, err := pty.Owned.Exited(0); err != nil || ended {
				return empty("unavailable")
			}
			if pty.Err() != nil {
				return empty("unavailable")
			}
			plain := screen.text()
			flat := strings.Join(strings.Fields(plain), " ")
			lower := strings.ToLower(flat)
			if plain != lastText {
				lastText = plain
				lastChange = time.Now()
			}
			if claudeDenied.MatchString(plain) || strings.Contains(lower, "select login method") || strings.Contains(lower, "sign in to claude") || strings.Contains(lower, "log in with your claude account") || strings.Contains(lower, "claude.ai/oauth/authorize") {
				return empty("needs_auth")
			}
			if !sent {
				if time.Since(started) > 45*time.Second {
					return empty("unavailable")
				}
				key, answer := "", ""
				switch {
				case !onboard["notes"] && strings.Contains(lower, "security notes") && strings.Contains(lower, "press enter to continue"):
					key = "notes"
					answer = "\r"
				case !onboard["terminal"] && strings.Contains(lower, "terminal setup") && strings.Contains(lower, "esc to skip"):
					key = "terminal"
					answer = "\x1b"
				case !onboard["theme"] && strings.Contains(lower, "choose the text style"):
					key = "theme"
					answer = "\r"
				case !onboard["browser-off"] && strings.Contains(lower, "keep browser tools off"):
					key = "browser-off"
					answer = "\x1b"
				case !onboard["trust"] && strings.Contains(lower, "trust this folder"):
					if trusts >= 3 {
						return empty("unavailable")
					}
					if !strings.Contains(flat, cwd) && !strings.Contains(flat, filepath.Base(cwd)) {
						return empty("unavailable")
					}
					if strings.Contains(lower, "\u276f no, exit") {
						if pty.Write([]byte("\x1b[B")) != nil {
							return empty("unavailable")
						}
						screen.clear()
						continue
					}
					if strings.Contains(lower, "\u276f yes, i trust") {
						key = "trust"
						answer = "\r"
						trusts++
					}
				case strings.Contains(plain, "\u276f\u00a0") || strings.Contains(plain, "\u276f "):
					if time.Since(lastChange) >= 300*time.Millisecond {
						screen.clear()
						if pty.Write([]byte("/usage\r")) != nil {
							return empty("unavailable")
						}
						sent = true
						panelStart = time.Now()
					}
				}
				if key != "" && !onboard[key] {
					onboard[key] = true
					screen.clear()
					if pty.Write([]byte(answer)) != nil {
						return empty("unavailable")
					}
				}
			} else {
				reading := parseClaudePanel(plain, time.Now().Unix())
				if reading.Status == "ready" && time.Since(lastChange) >= 300*time.Millisecond {
					return reading
				}
				if strings.Contains(lower, "failed to load usage data") && !retried && time.Since(lastChange) >= 300*time.Millisecond {
					retried = true
					screen.clear()
					if pty.Write([]byte("r")) != nil {
						return empty("unavailable")
					}
				}
				if time.Since(panelStart) > 20*time.Second {
					return empty("unavailable")
				}
			}
		}
	}
}

// Only the fresh session's known record is removed, confined to this profile.
// Shared project directories and any pre-existing sessions are left untouched.
func (l *Local) dropClaudeSession(cwd, session string) {
	if uuid.Validate(session) != nil {
		return
	}
	home, err := openQuotaHome(l.home)
	if err != nil {
		return
	}
	defer home.Close()
	root, err := home.OpenRoot(".claude")
	if err != nil {
		return
	}
	defer root.Close()
	paths := []string{cwd}
	if canonical, e := filepath.EvalSymlinks(cwd); e == nil && canonical != cwd {
		paths = append(paths, canonical)
	}
	for _, path := range paths {
		var project strings.Builder
		for _, r := range strings.TrimPrefix(path, `\\?\`) {
			if (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (r >= '0' && r <= '9') {
				project.WriteRune(r)
			} else {
				project.WriteByte('-')
			}
		}
		_ = root.Remove(filepath.Join("projects", project.String(), session+".jsonl"))
	}
}
