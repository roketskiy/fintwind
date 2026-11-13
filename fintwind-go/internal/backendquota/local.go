package backendquota

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

type Local struct {
	home                     string
	http                     Doer
	mu                       sync.Mutex
	identity                 [32]byte
	last, good               *Quota
	nextAuto, nextManual     int64
	claudeMu                 sync.Mutex
	claudeIdentity           [32]byte
	claudeAccount            [32]byte
	claudeLast, claudeGood   *Quota
	claudeAuto, claudeManual int64
}

func NewLocal(home string, transport Doer) *Local {
	if transport == nil {
		transport = New(nil, nil).http
	}
	return &Local{home: home, http: transport}
}

// Anchor at the profile, not home itself: opening home directly would accept a
// replaced home symlink before the credential-file traversal checks can run.
func openQuotaHome(home string) (*os.Root, error) {
	home = filepath.Clean(home)
	if !filepath.IsAbs(home) {
		return nil, errors.New("absolute isolated quota home required")
	}
	parent := filepath.Dir(home)
	relative, err := filepath.Rel(parent, home)
	if err != nil || relative == "." || !filepath.IsLocal(relative) {
		return nil, errors.New("invalid isolated quota home")
	}
	profile, err := os.OpenRoot(parent)
	if err != nil {
		return nil, err
	}
	defer profile.Close()
	return profile.OpenRoot(relative)
}
func jwtClaims(token string) object {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return object{}
	}
	raw, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return object{}
	}
	claims, err := decode(raw)
	if err != nil {
		return object{}
	}
	return claims
}
func (l *Local) readCodex() (object, [32]byte, string) {
	var zero [32]byte
	// Go daemon is agent-only. Never consult inherited CODEX_HOME or the
	// machine's real profile; only this explicit isolated home's .codex exists.
	root, err := openQuotaHome(l.home)
	if err != nil {
		return nil, zero, "unavailable"
	}
	defer root.Close()
	store := "file"
	if file, err := root.Open(filepath.Join(".codex", "config.toml")); err == nil {
		raw, e := io.ReadAll(io.LimitReader(file, (1<<20)+1))
		file.Close()
		if e != nil || len(raw) > 1<<20 {
			return nil, zero, "unavailable"
		}
		for _, line := range strings.Split(string(raw), "\n") {
			key, value, ok := strings.Cut(strings.TrimSpace(line), "=")
			if ok && strings.TrimSpace(key) == "cli_auth_credentials_store" {
				store = strings.Trim(strings.TrimSpace(value), "\"'")
			}
		}
	}
	file, err := root.Open(filepath.Join(".codex", "auth.json"))
	if os.IsNotExist(err) {
		if store != "file" {
			return nil, zero, "unavailable"
		}
		return nil, zero, "no_credential"
	}
	if err != nil {
		return nil, zero, "unavailable"
	}
	defer file.Close()
	raw, err := io.ReadAll(io.LimitReader(file, (1<<20)+1))
	if err != nil || len(raw) > 1<<20 {
		return nil, zero, "unavailable"
	}
	auth, err := decode(raw)
	if err != nil {
		return nil, zero, "unavailable"
	}
	if text(auth["auth_mode"]) == "apikey" {
		return nil, zero, "unsupported"
	}
	access := text(at(auth, "tokens", "access_token"))
	if access == "" {
		return nil, zero, "no_credential"
	}
	claims := jwtClaims(access)
	if expires := number(claims["exp"]); expires != nil && *expires <= float64(time.Now().Unix()) {
		return nil, zero, "needs_auth"
	}
	return auth, sha256.Sum256(raw), ""
}

func (l *Local) Fetch(ctx context.Context, provider string, force bool) Quota {
	if provider != "openai" && provider != "anthropic" {
		return empty("unsupported")
	}
	if provider == "anthropic" {
		return l.claude(ctx, force)
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	auth, identity, status := l.readCodex()
	if status != "" {
		l.last = nil
		l.good = nil
		l.identity = [32]byte{}
		return empty(status)
	}
	if identity != l.identity {
		l.identity = identity
		l.good = nil
		l.last = nil
		l.nextAuto = 0
		l.nextManual = 0
	}
	now := time.Now().Unix()
	floor := l.nextAuto
	if force {
		floor = l.nextManual
	}
	if now < floor && l.last != nil {
		return *l.last
	}
	token := text(at(auth, "tokens", "access_token"))
	account := text(at(auth, "tokens", "account_id"))
	claims := jwtClaims(token)
	if account == "" {
		account = text(at(claims, "https://api.openai.com/auth", "chatgpt_account_id"))
	}
	for _, value := range []string{token, account} {
		for _, r := range value {
			if r < 32 || r == 127 || r == '"' || r == '\\' {
				return empty("needs_auth")
			}
		}
	}
	requestCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	request, err := http.NewRequestWithContext(requestCtx, "GET", "https://chatgpt.com/backend-api/wham/usage", nil)
	if err != nil {
		return empty("unavailable")
	}
	request.Header.Set("Authorization", "Bearer "+token)
	request.Header.Set("Accept", "application/json")
	request.Header.Set("User-Agent", "fintwind")
	if account != "" {
		request.Header.Set("chatgpt-account-id", account)
	}
	reading := empty("unavailable")
	response, err := l.http.Do(request)
	if err == nil {
		raw, readErr := io.ReadAll(io.LimitReader(response.Body, (8<<20)+1))
		response.Body.Close()
		if readErr == nil && len(raw) <= 8<<20 {
			switch {
			case response.StatusCode >= 200 && response.StatusCode < 300:
				reading = parseCodex(raw, now)
			case response.StatusCode == 401:
				lower := strings.ToLower(string(raw))
				if strings.Contains(lower, "token_expired") || strings.Contains(lower, "token_revoked") || strings.Contains(lower, "refresh") {
					reading.Status = "needs_auth"
				}
			case response.StatusCode == 403:
				reading.Status = "access_denied"
			case response.StatusCode == 429:
				reading.Status = "rate_limited"
			}
		}
	}
	_, verified, status := l.readCodex()
	if status != "" || verified != identity {
		l.last = nil
		l.good = nil
		l.identity = [32]byte{}
		return empty("unavailable")
	}
	reading.Service = ptr("Codex")
	label := text(jwtClaims(text(at(auth, "tokens", "id_token")))["email"])
	if label == "" {
		label = text(claims["email"])
	}
	if label == "" {
		label = "ChatGPT"
	}
	reading.Account = &label
	if reading.Plan == nil {
		if plan := text(at(claims, "https://api.openai.com/auth", "chatgpt_plan_type")); plan != "" {
			reading.Plan = &plan
		}
	}
	now = time.Now().Unix()
	l.nextAuto = now + 60
	l.nextManual = now + 15
	if reading.Status == "rate_limited" {
		l.nextManual = now + 60
	}
	if reading.Status == "ready" {
		reading.MeasuredAt = &now
		copy := reading
		l.good = &copy
	} else if reading.Status == "unavailable" || reading.Status == "rate_limited" {
		if l.good != nil && l.good.MeasuredAt != nil && now-*l.good.MeasuredAt < 86400 {
			status := reading.Status
			reading = *l.good
			reading.Status = status
		}
	} else {
		l.good = nil
	}
	if reading.Status != "ready" {
		reading.RetryAt = ptr(l.nextAuto)
	}
	copy := reading
	l.last = &copy
	return reading
}

func parseCodex(raw []byte, now int64) Quota {
	body, err := decode(raw)
	if err != nil {
		return empty("unavailable")
	}
	q := empty("ready")
	if plan := text(body["plan_type"]); plan != "" {
		q.Plan = &plan
	}
	for _, key := range []string{"primary_window", "secondary_window"} {
		row := obj(at(body, "rate_limit", key))
		used := percent(row["used_percent"])
		if used == nil {
			continue
		}
		var span uint64
		if n := number(row["limit_window_seconds"]); n != nil && *n > 0 {
			span = uint64(*n)
		}
		kind := "rolling"
		if span == 604800 {
			kind = "weekly"
		}
		reset := timestamp(row["reset_at"])
		if reset == nil {
			if n := number(row["reset_after_seconds"]); n != nil && *n > 0 {
				reset = ptr(now + int64(*n))
			}
		}
		q.Windows = append(q.Windows, window(key, kind, used, reset, span))
	}
	credits := obj(body["credits"])
	if credits["unlimited"] == true {
		q.Balances = append(q.Balances, Balance{Unit: "credits", Remaining: "", Unlimited: true})
	} else if credits["has_credits"] == true {
		if balance := money(credits["balance"]); balance != nil {
			q.Balances = append(q.Balances, Balance{Unit: "credits", Remaining: *balance})
		}
	}
	if len(q.Windows) == 0 && len(q.Balances) == 0 {
		q.Status = "no_data"
	}
	return q
}

// Claude's usage needs its own terminal protocol. No server-credential
// fallback is permitted while that local terminal adapter is not available.
func (l *Local) claude(ctx context.Context, force bool) Quota {
	return l.fetchClaude(ctx, force)
}
