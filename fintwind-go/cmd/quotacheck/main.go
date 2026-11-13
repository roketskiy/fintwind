package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	quota "fintwind-go/internal/backendquota"
	"fintwind-go/internal/ownedprocess"
)

type fixture struct {
	provider, key              string
	calls, vendorCalls         int
	status                     int
	body                       string
	lastAuthorization, lastURL string
	envName                    string
}

func (f *fixture) RoundTrip(_ context.Context, method, path string, _ any) (json.RawMessage, error) {
	f.calls++
	if strings.HasPrefix(path, "/api/integration/") {
		if f.envName != "" {
			return json.Marshal(map[string]any{"data": map[string]any{"id": f.provider, "connections": []any{map[string]any{"type": "env", "name": f.envName}}}})
		}
		if f.key == "" {
			return json.Marshal(map[string]any{"data": map[string]any{"id": f.provider, "connections": []any{}}})
		}
		return json.Marshal(map[string]any{"data": map[string]any{"id": f.provider, "connections": []any{map[string]any{"type": "credential", "id": "fixture", "status": map[string]string{"status": "connected"}}}}})
	}
	return json.Marshal(map[string]any{"data": []any{map[string]any{"id": "fixture", "integrationID": f.provider, "active": true, "value": map[string]any{"type": "key", "key": f.key}}}})
}
func (f *fixture) Do(request *http.Request) (*http.Response, error) {
	f.vendorCalls++
	f.lastAuthorization = request.Header.Get("Authorization")
	f.lastURL = request.URL.String()
	return &http.Response{StatusCode: f.status, Header: http.Header{}, Body: io.NopCloser(strings.NewReader(f.body))}, nil
}
func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}
func run() (err error) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	defer func() {
		report := map[string]any{"passed": err == nil, "checks": checks, "realCredentialsRead": false, "realNetworkUsed": false}
		if err != nil {
			report["error"] = err.Error()
		}
		raw, e := json.MarshalIndent(report, "", "  ")
		if e == nil {
			e = os.WriteFile(filepath.Join(round, "quota-result.json"), raw, 0o600)
		}
		err = errors.Join(err, e)
		fmt.Println("[REPORT]", filepath.Join(round, "quota-result.json"))
	}()
	api := &fixture{provider: "deepseek", key: "sentinel", status: 200, body: `{"balance_infos":[{"currency":"CNY","total_balance":"0.001234567890123456789"}]}`}
	service := quota.New(api, nil)
	if q := service.Fetch(ctx, "fixture", api, "unknown-provider", false); q.Status != "unsupported" || api.calls != 0 || api.vendorCalls != 0 {
		return errors.New("unsupported provider made network requests")
	}
	checks["unsupported-fails-without-io"] = true
	if q := service.Fetch(ctx, "fixture", api, "anthropic", false); q.Status != "unavailable" || api.calls != 0 {
		return errors.New("local source touched server credentials")
	}
	checks["local-quota-never-server-credentials"] = true
	api.key = ""
	if q := service.Fetch(ctx, "fixture", api, "deepseek", false); q.Status != "no_credential" || api.vendorCalls != 0 {
		return errors.New("missing credential made upstream request")
	}
	checks["missing-credential-no-vendor-request"] = true
	api.key = "bad\r\nkey"
	if q := service.Fetch(ctx, "fixture", api, "deepseek", false); q.Status != "needs_auth" || api.vendorCalls != 0 {
		return errors.New("unsafe key reached upstream")
	}
	checks["header-injection-refused"] = true
	api.key = "sentinel"
	q := service.Fetch(ctx, "fixture", api, "deepseek", false)
	if q.Status != "ready" || len(q.Balances) != 1 || q.Balances[0].Remaining != "0.001234567890123456789" || api.lastAuthorization != "Bearer sentinel" {
		return errors.New("balance precision or authorization mismatch")
	}
	checks["decimal-precision-preserved"] = true
	_ = service.Fetch(ctx, "fixture", api, "deepseek", true)
	if api.vendorCalls != 1 {
		return errors.New("manual cache repeated vendor request")
	}
	checks["cache-cooldown"] = true
	api.key = ""
	if q := service.Fetch(ctx, "fixture", api, "deepseek", false); q.Status != "no_credential" || len(q.Balances) != 0 {
		return errors.New("logout retained another account balance")
	}
	checks["logout-clears-good-cache"] = true
	api.provider = "zhipuai-coding-plan"
	api.key = "sentinel-plan"
	api.body = `{"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":20,"nextResetTime":2000000000000}]}}`
	q = service.Fetch(ctx, "fixture", api, api.provider, false)
	if q.Status != "ready" || api.lastAuthorization != "sentinel-plan" || api.lastURL != "https://open.bigmodel.cn/api/monitor/usage/quota/limit" || len(q.Windows) != 1 || q.Windows[0].SpanSeconds == nil || *q.Windows[0].SpanSeconds != 18000 {
		return errors.New("plan endpoint/header/window contract mismatch")
	}
	checks["zhipu-plan-contract"] = true
	for _, scenario := range []struct {
		code int
		want string
	}{{401, "needs_auth"}, {403, "access_denied"}, {429, "rate_limited"}, {500, "unavailable"}} {
		api.provider = "deepseek"
		api.key = "status-sentinel"
		api.status = scenario.code
		q := quota.New(api, nil).Fetch(ctx, "status", api, "deepseek", false)
		if q.Status != scenario.want || len(q.Balances) != 0 {
			return errors.New("quota HTTP error classification mismatch")
		}
		checks[fmt.Sprintf("http-status-%d", scenario.code)] = true
	}
	api.status = 200
	for _, scenario := range []struct{ provider, body string }{
		{"google", `{"buckets":[{"modelId":"literal","remainingFraction":0.5}]}`},
		{"github-copilot", `{"quota_snapshots":{"chat":{"unlimited":true}}}`},
		{"openrouter", `{"data":{"total_credits":12,"total_usage":2}}`},
		{"siliconflow-cn", `{"data":{"totalBalance":0}}`},
		{"moonshotai", `{"data":{"available_balance":4}}`},
		{"zhipuai", `{"success":true,"code":200,"data":{"availableBalance":"0"}}`},
		{"minimax", `{"base_resp":{"status_code":0},"model_remains":[{"model_name":"general","current_interval_status":2}]}`},
		{"kimi-code-plan-cn", `{"usages":{"limit_5h":{"used_ratio":0.3}}}`},
		{"opencode-go", `{"usage":{"weekly":{"percent":25}}}`},
		{"opencode", `{"data":{"go":{"rolling":{"percent":0}}}}`},
		{"commandcode", `{"credits":{"monthlyCredits":10}}`},
		{"ollama-cloud", `{"included":{"session":{"remaining_percent":50}}}`},
		{"stepfun", `{"object":"account","balance":"2.00"}`},
	} {
		q := quota.Parse(scenario.provider, []byte(scenario.body))
		if q.Status != "ready" {
			return fmt.Errorf("adapter %s fixture returned %s", scenario.provider, q.Status)
		}
		checks["adapter-"+scenario.provider] = true
	}
	home := filepath.Join(round, "home")
	authDir := filepath.Join(home, ".codex")
	if err := os.MkdirAll(authDir, 0o700); err != nil {
		return err
	}
	payload, _ := json.Marshal(map[string]any{"exp": time.Now().Unix() + 3600, "email": "fixture@example.invalid"})
	access := "e30." + base64.RawURLEncoding.EncodeToString(payload) + ".signature"
	auth, _ := json.Marshal(map[string]any{"tokens": map[string]string{"access_token": access, "account_id": "fixture-account"}})
	if err := os.WriteFile(filepath.Join(authDir, "auth.json"), auth, 0o600); err != nil {
		return err
	}
	api.status = 200
	api.body = `{"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_after_seconds":60}}}`
	local := quota.NewLocal(home, api)
	q = local.Fetch(ctx, "openai", false)
	if q.Status != "ready" || q.Service == nil || *q.Service != "Codex" || api.lastURL != "https://chatgpt.com/backend-api/wham/usage" {
		return errors.New("isolated local Codex quota mismatch")
	}
	unchanged, err := os.ReadFile(filepath.Join(authDir, "auth.json"))
	if err != nil || string(unchanged) != string(auth) {
		return errors.New("local quota changed auth file")
	}
	checks["codex-local-readonly-isolated"] = true
	api.provider = "deepseek"
	api.envName = "DEEPSEEK_API_KEY"
	api.body = `{"balance_infos":[{"currency":"CNY","total_balance":"9.00"}]}`
	lookups := 0
	envService := quota.NewWithEnvironment(api, nil, func(name string) (string, bool) {
		lookups++
		if name != "DEEPSEEK_API_KEY" {
			return "", false
		}
		return "isolated-env-sentinel", true
	})
	q = envService.Fetch(ctx, "env-fixture", api, "deepseek", false)
	if q.Status != "ready" || api.lastAuthorization != "Bearer isolated-env-sentinel" || lookups != 2 {
		return errors.New("selected environment connection not resolved and rechecked")
	}
	checks["selected-environment-key-only"] = true
	api.envName = "UNRELATED_SECRET"
	before := lookups
	q = envService.Fetch(ctx, "env-fixture", api, "deepseek", false)
	if q.Status != "no_credential" || lookups != before || len(q.Balances) != 0 {
		return errors.New("unregistered environment key queried")
	}
	checks["unregistered-environment-key-never-read"] = true
	planService := quota.NewWithEnvironment(api, nil, func(string) (string, bool) { return "", false })
	if usage, err := planService.PlanUsage(ctx, home); err != nil || usage != nil {
		return errors.New("missing plan credential should return null usage")
	}
	checks["plan-no-credential-null"] = true
	planDir := filepath.Join(home, "opencode")
	if err := os.MkdirAll(planDir, 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(planDir, "auth.json"), []byte(`{"opencode-go":{"type":"api","key":"isolated-plan-key"}}`), 0o600); err != nil {
		return err
	}
	api.body = `{"usage":{"rolling":{"percent":0},"weekly":{"percent":50}}}`
	usage, err := planService.PlanUsage(ctx, home)
	if err != nil {
		return err
	}
	if usage == nil || api.lastURL != "https://opencode.ai/zen/go/v1/usage" || api.lastAuthorization != "Bearer isolated-plan-key" {
		return errors.New("plan usage read wrong provider credential")
	}
	checks["plan-exact-go-endpoint-legacy-auth"] = true
	return nil
}
