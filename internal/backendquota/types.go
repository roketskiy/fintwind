// Package backendquota ports sanitized allowance mappings from CodexBar and
// Magpie (MIT, snapshot 43849d51). See docs/provider-quota-upstream-license.txt.
// Credentials and upstream error bodies never leave the daemon boundary.
package backendquota

import (
	"bytes"
	"encoding/json"
	"math"
	"strconv"
	"strings"
	"time"
)

type Quota struct {
	Status     string    `json:"status"`
	Plan       *string   `json:"plan"`
	Service    *string   `json:"service"`
	Account    *string   `json:"account"`
	Windows    []Window  `json:"windows"`
	Balances   []Balance `json:"balances"`
	MeasuredAt *int64    `json:"measuredAt"`
	RetryAt    *int64    `json:"retryAt"`
}
type Window struct {
	ID          string   `json:"id"`
	Kind        string   `json:"kind"`
	Model       *string  `json:"model"`
	SpanSeconds *uint64  `json:"spanSeconds"`
	UsedPercent *float64 `json:"usedPercent"`
	Remaining   *string  `json:"remaining"`
	Limit       *string  `json:"limit"`
	ResetsAt    *int64   `json:"resetsAt"`
	Unlimited   bool     `json:"unlimited"`
}
type Balance struct {
	Unit      string `json:"unit"`
	Remaining string `json:"remaining"`
	Unlimited bool   `json:"unlimited"`
}
type object map[string]any

func empty(status string) Quota {
	return Quota{Status: status, Windows: []Window{}, Balances: []Balance{}}
}
func ptr[T any](value T) *T { return &value }
func decode(raw []byte) (object, error) {
	var value object
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	err := decoder.Decode(&value)
	return value, err
}
func obj(value any) object {
	if v, ok := value.(map[string]any); ok {
		return v
	}
	if v, ok := value.(object); ok {
		return v
	}
	return object{}
}
func at(value any, keys ...string) any {
	for _, key := range keys {
		value = obj(value)[key]
	}
	return value
}
func text(value any) string { v, _ := value.(string); return v }
func list(value any) []any  { v, _ := value.([]any); return v }
func number(value any) *float64 {
	var n float64
	var err error
	switch v := value.(type) {
	case json.Number:
		n, err = v.Float64()
	case string:
		n, err = strconv.ParseFloat(v, 64)
	case float64:
		n = v
	default:
		return nil
	}
	if err != nil || math.IsNaN(n) || math.IsInf(n, 0) {
		return nil
	}
	return &n
}
func percent(value any) *float64 {
	n := number(value)
	if n == nil || *n < 0 {
		return nil
	}
	return ptr(math.Min(*n, 100))
}
func money(value any) *string {
	var s string
	switch v := value.(type) {
	case string:
		s = v
	case json.Number:
		s = string(v)
	default:
		return nil
	}
	digits := strings.TrimPrefix(s, "-")
	if digits == "" || strings.Count(digits, ".") > 1 {
		return nil
	}
	hasDigit := false
	for _, r := range digits {
		if r >= '0' && r <= '9' {
			hasDigit = true
		} else if r != '.' {
			return nil
		}
	}
	if !hasDigit {
		return nil
	}
	return &s
}
func timestamp(value any) *int64 {
	if s, ok := value.(string); ok {
		if t, err := time.Parse(time.RFC3339, s); err == nil {
			return ptr(t.Unix())
		}
	}
	var s string
	switch v := value.(type) {
	case string:
		s = v
	case json.Number:
		s = string(v)
	default:
		return nil
	}
	n, err := strconv.ParseInt(s, 10, 64)
	if err != nil || n <= 0 {
		return nil
	}
	if n >= 1000000000000 {
		n /= 1000
	}
	return &n
}
func window(id, kind string, used *float64, reset *int64, span uint64) Window {
	w := Window{ID: id, Kind: kind, UsedPercent: used, ResetsAt: reset}
	if span > 0 {
		w.SpanSeconds = &span
	}
	return w
}
func addCount(w *Window, limit, used, remaining any) {
	total := number(limit)
	if total == nil || *total <= 0 {
		return
	}
	left := number(remaining)
	if left == nil {
		if n := number(used); n != nil {
			left = ptr(*total - *n)
		}
	}
	if left != nil {
		w.UsedPercent = ptr(math.Max(0, math.Min(100, (*total-*left) / *total * 100)))
		w.Remaining = ptr(strconv.FormatFloat(math.Max(0, *left), 'f', -1, 64))
		w.Limit = money(limit)
	}
}

var endpoints = map[string]string{
	"github-copilot": "https://api.github.com/copilot_internal/user", "google": "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota",
	"zhipuai-coding-plan": "https://open.bigmodel.cn/api/monitor/usage/quota/limit", "zai-coding-plan": "https://api.z.ai/api/monitor/usage/quota/limit",
	"zhipuai": "https://open.bigmodel.cn/api/biz/account/query-customer-account-report", "zai": "https://api.z.ai/api/biz/account/query-customer-account-report",
	"kimi-code-plan-cn": "https://api.kimi.com/coding/v1/usages", "kimi-code-plan-global": "https://api.kimi.ai/coding/v1/usages",
	"commandcode": "https://api.commandcode.ai/alpha/billing/credits", "ollama-cloud": "https://ollama.com/api/balance", "openrouter": "https://openrouter.ai/api/v1/credits",
	"siliconflow": "https://api.siliconflow.com/v1/user/info", "siliconflow-cn": "https://api.siliconflow.cn/v1/user/info", "moonshotai": "https://api.moonshot.ai/v1/users/me/balance", "moonshotai-cn": "https://api.moonshot.cn/v1/users/me/balance",
	"opencode-go": "https://opencode.ai/zen/go/v1/usage", "minimax": "https://api.minimax.io/v1/token_plan/remains", "minimax-coding-plan": "https://api.minimax.io/v1/token_plan/remains", "minimax-cn": "https://api.minimax.cn/v1/token_plan/remains", "minimax-cn-coding-plan": "https://api.minimaxi.com/v1/token_plan/remains",
	"deepseek": "https://api.deepseek.com/user/balance", "stepfun": "https://api.stepfun.com/v1/accounts", "stepfun-step-plan": "https://api.stepfun.com/v1/accounts", "stepfun-ai": "https://api.stepfun.ai/v1/accounts", "stepfun-ai-step-plan": "https://api.stepfun.ai/v1/accounts",
}

func Supported(id string) bool {
	return id == "anthropic" || id == "openai" || id == "opencode" || endpoints[id] != ""
}
