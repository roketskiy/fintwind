package backendquota

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

type API interface {
	RoundTrip(context.Context, string, string, any) (json.RawMessage, error)
}
type Doer interface {
	Do(*http.Request) (*http.Response, error)
}
type LocalSource func(context.Context, string, bool) Quota
type cached struct {
	mu                   sync.Mutex
	identity             [32]byte
	last, good           *Quota
	nextAuto, nextManual int64
}
type Service struct {
	mu          sync.Mutex
	entries     map[string]*cached
	http        Doer
	local       LocalSource
	now         func() time.Time
	environment func(string) (string, bool)
}

func New(transport Doer, local LocalSource) *Service {
	if transport == nil {
		transport = &http.Client{Timeout: 15 * time.Second, Transport: &http.Transport{Proxy: http.ProxyFromEnvironment, MaxIdleConns: 8, IdleConnTimeout: 30 * time.Second}, CheckRedirect: func(*http.Request, []*http.Request) error { return errors.New("quota redirects refused") }}
	}
	return &Service{entries: map[string]*cached{}, http: transport, local: local, now: time.Now, environment: os.LookupEnv}
}

// NewWithEnvironment lets an isolated host supply its captured engine
// environment. Only the selected provider's documented key can be queried.
func NewWithEnvironment(transport Doer, local LocalSource, environment func(string) (string, bool)) *Service {
	s := New(transport, local)
	s.environment = environment
	return s
}

func quotaEnvironmentName(provider string) string {
	switch provider {
	case "deepseek":
		return "DEEPSEEK_API_KEY"
	case "zhipuai", "zhipuai-coding-plan", "zai", "zai-coding-plan":
		return "ZHIPU_API_KEY"
	case "kimi-code-plan-cn", "kimi-code-plan-global":
		return "KIMI_API_KEY"
	case "minimax", "minimax-cn", "minimax-coding-plan", "minimax-cn-coding-plan":
		return "MINIMAX_API_KEY"
	case "stepfun", "stepfun-ai", "stepfun-step-plan", "stepfun-ai-step-plan":
		return "STEPFUN_API_KEY"
	case "ollama-cloud":
		return "OLLAMA_API_KEY"
	case "openrouter":
		return "OPENROUTER_API_KEY"
	case "siliconflow":
		return "SILICONFLOW_API_KEY"
	case "siliconflow-cn":
		return "SILICONFLOW_CN_API_KEY"
	case "moonshotai", "moonshotai-cn":
		return "MOONSHOT_API_KEY"
	case "opencode-go":
		return "OPENCODE_API_KEY"
	case "commandcode":
		return "CMD_API_KEY"
	default:
		return ""
	}
}
func unwrap(raw json.RawMessage) (json.RawMessage, error) {
	var envelope struct {
		Data json.RawMessage `json:"data"`
	}
	if len(bytes.TrimSpace(raw)) > 0 && bytes.TrimSpace(raw)[0] == '[' {
		return raw, nil
	}
	if err := json.Unmarshal(raw, &envelope); err != nil {
		return nil, err
	}
	if len(envelope.Data) > 0 {
		return envelope.Data, nil
	}
	return raw, nil
}

// selected reads only the first server-selected connection. It never chooses
// a second saved account just because that one has usable credentials.
func selected(ctx context.Context, api API, provider string, environment func(string) (string, bool)) (json.RawMessage, string) {
	raw, err := api.RoundTrip(ctx, "GET", "/api/integration/"+url.PathEscape(provider), nil)
	if err != nil {
		return nil, "unavailable"
	}
	raw, err = unwrap(raw)
	if err != nil {
		return nil, "unavailable"
	}
	integration, err := decode(raw)
	if err != nil || text(integration["id"]) != provider {
		return nil, "unavailable"
	}
	connections := list(integration["connections"])
	if len(connections) == 0 {
		return nil, "no_credential"
	}
	connection := obj(connections[0])
	if text(at(connection, "status", "status")) == "needs_auth" {
		return nil, "needs_auth"
	}
	if provider == "opencode" {
		raw, _ := json.Marshal(connections)
		return raw, ""
	}
	if text(connection["type"]) == "env" {
		name := quotaEnvironmentName(provider)
		if name == "" || text(connection["name"]) != name || environment == nil {
			return nil, "no_credential"
		}
		key, ok := environment(name)
		if !ok || strings.TrimSpace(key) == "" {
			return nil, "no_credential"
		}
		credential, _ := json.Marshal(map[string]any{"id": "env:" + name, "integrationID": provider, "active": true, "value": map[string]string{"type": "key", "key": key}})
		return credential, ""
	}
	if text(connection["type"]) != "credential" {
		return nil, "no_credential"
	}
	raw, err = api.RoundTrip(ctx, "GET", "/api/credential", nil)
	if err != nil {
		return nil, "unavailable"
	}
	raw, err = unwrap(raw)
	if err != nil {
		return nil, "unavailable"
	}
	var rows []json.RawMessage
	if json.Unmarshal(raw, &rows) != nil {
		return nil, "unavailable"
	}
	for _, row := range rows {
		credential, err := decode(row)
		if err != nil {
			continue
		}
		if text(credential["id"]) == text(connection["id"]) && text(credential["integrationID"]) == provider {
			credential["active"] = true
			encoded, _ := json.Marshal(credential)
			return encoded, ""
		}
	}
	return nil, "no_credential"
}

func requestFor(ctx context.Context, provider string, credential json.RawMessage, now int64) (*http.Request, string) {
	entry, err := decode(credential)
	if err != nil {
		return nil, "needs_auth"
	}
	value := obj(entry["value"])
	kind := text(value["type"])
	flexible := map[string]bool{"commandcode": true, "ollama-cloud": true, "openrouter": true, "siliconflow": true, "siliconflow-cn": true, "moonshotai": true, "moonshotai-cn": true, "opencode-go": true, "zhipuai": true, "zai": true, "minimax": true, "minimax-cn": true, "stepfun-ai": true, "stepfun-ai-step-plan": true, "stepfun-step-plan": true}
	oauth := provider == "google" || provider == "github-copilot" || flexible[provider] && kind == "oauth"
	if flexible[provider] {
		if kind != "key" && kind != "oauth" {
			return nil, "unsupported"
		}
	} else if oauth && kind != "oauth" || !oauth && kind != "key" {
		return nil, "unsupported"
	}
	if oauth && provider != "github-copilot" {
		expires := number(value["expires"])
		if expires == nil || *expires <= float64(now)*1000 {
			return nil, "needs_auth"
		}
	}
	token := text(value["key"])
	if oauth {
		token = text(value["access"])
	}
	if token == "" {
		return nil, "needs_auth"
	}
	for _, r := range token {
		if r < 32 || r == 127 || r == '"' || r == '\\' {
			return nil, "needs_auth"
		}
	}
	endpoint := endpoints[provider]
	if endpoint == "" {
		return nil, "unsupported"
	}
	method := "GET"
	var body []byte
	if provider == "google" {
		metadata := obj(value["metadata"])
		project := ""
		for _, key := range []string{"projectId", "projectID", "project"} {
			if project = text(metadata[key]); project != "" {
				break
			}
		}
		if project == "" {
			return nil, "unsupported"
		}
		method = "POST"
		body, _ = json.Marshal(map[string]string{"project": project})
	}
	if provider == "github-copilot" && at(value, "metadata", "enterpriseUrl") != nil {
		return nil, "unsupported"
	}
	request, err := http.NewRequestWithContext(ctx, method, endpoint, bytes.NewReader(body))
	if err != nil {
		return nil, "unavailable"
	}
	auth := "Bearer " + token
	if provider == "zhipuai-coding-plan" || provider == "zai-coding-plan" {
		auth = token
	}
	request.Header.Set("Authorization", auth)
	request.Header.Set("Accept", "application/json")
	request.Header.Set("User-Agent", "fintwind")
	if body != nil {
		request.Header.Set("Content-Type", "application/json")
	}
	if provider == "github-copilot" {
		request.Header.Set("X-GitHub-Api-Version", "2025-04-01")
	}
	return request, ""
}

func (s *Service) Fetch(ctx context.Context, source string, api API, provider string, force bool) Quota {
	if !Supported(provider) {
		return empty("unsupported")
	}
	if provider == "anthropic" || provider == "openai" {
		if s.local == nil {
			return empty("unavailable")
		}
		return s.local(ctx, provider, force)
	}
	if api == nil {
		return empty("unavailable")
	}
	s.mu.Lock()
	key := source + "\x00" + provider
	entry := s.entries[key]
	if entry == nil {
		if len(s.entries) >= 256 {
			s.mu.Unlock()
			return empty("unavailable")
		}
		entry = &cached{}
		s.entries[key] = entry
	}
	s.mu.Unlock()
	entry.mu.Lock()
	defer entry.mu.Unlock()
	credential, status := selected(ctx, api, provider, s.environment)
	if status != "" {
		entry.identity = [32]byte{}
		entry.good = nil
		entry.last = nil
		return empty(status)
	}
	identity := sha256.Sum256(credential)
	if identity != entry.identity {
		entry.identity = identity
		entry.good = nil
		entry.last = nil
		entry.nextAuto = 0
		entry.nextManual = 0
	}
	now := s.now().Unix()
	floor := entry.nextAuto
	if force {
		floor = entry.nextManual
	}
	if now < floor && entry.last != nil {
		return *entry.last
	}
	reading := empty("unavailable")
	retrySeconds := int64(0)
	if provider == "opencode" {
		raw, err := api.RoundTrip(ctx, "GET", "/api/experimental/usage/limits", nil)
		if err == nil {
			reading = Parse(provider, raw)
		}
	} else {
		request, status := requestFor(ctx, provider, credential, now)
		if status != "" {
			entry.good = nil
			entry.last = nil
			return empty(status)
		}
		requestCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
		defer cancel()
		request = request.WithContext(requestCtx)
		response, err := s.http.Do(request)
		if err == nil {
			raw, readErr := io.ReadAll(io.LimitReader(response.Body, (8<<20)+1))
			response.Body.Close()
			if readErr == nil && len(raw) <= 8<<20 {
				switch {
				case response.StatusCode >= 200 && response.StatusCode < 300:
					reading = Parse(provider, raw)
				case response.StatusCode == 401:
					reading = empty("needs_auth")
				case response.StatusCode == 403:
					reading = empty("access_denied")
				case response.StatusCode == 429:
					reading = empty("rate_limited")
					if n, err := strconv.ParseInt(strings.TrimSpace(response.Header.Get("Retry-After")), 10, 64); err == nil && n > 0 {
						retrySeconds = min(n, 86400)
					} else if t, err := http.ParseTime(response.Header.Get("Retry-After")); err == nil {
						retrySeconds = min(max(0, t.Unix()-now), 86400)
					}
				}
			}
		}
	}
	verified, status := selected(ctx, api, provider, s.environment)
	if status != "" || sha256.Sum256(verified) != identity {
		entry.good = nil
		entry.last = nil
		entry.identity = [32]byte{}
		return empty("unavailable")
	}
	now = s.now().Unix()
	entry.nextAuto = now + 60
	entry.nextManual = now + 15
	if reading.Status == "rate_limited" {
		entry.nextAuto = now + max(60, retrySeconds)
		entry.nextManual = entry.nextAuto
	}
	if reading.Status == "ready" {
		reading.MeasuredAt = &now
		copy := reading
		entry.good = &copy
	} else if reading.Status == "unavailable" || reading.Status == "rate_limited" {
		if entry.good != nil && entry.good.MeasuredAt != nil && now-*entry.good.MeasuredAt < 86400 {
			status := reading.Status
			reading = *entry.good
			reading.Status = status
		}
	} else {
		entry.good = nil
	}
	if reading.Status != "ready" {
		reading.RetryAt = ptr(entry.nextAuto)
	}
	copy := reading
	entry.last = &copy
	return reading
}
