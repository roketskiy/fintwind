package backendquota

import (
	"fmt"
	"math"
	"strings"
	"time"
)

func Parse(provider string, raw []byte) Quota {
	body, err := decode(raw)
	if err != nil {
		return empty("unavailable")
	}
	q := empty("ready")
	switch provider {
	case "opencode-go", "opencode":
		usage := obj(body["usage"])
		if provider == "opencode" {
			data := obj(body["data"])
			if len(data) == 0 {
				data = body
			}
			usage = obj(data["go"])
			if len(usage) > 0 {
				q.Service = ptr("OpenCode Go")
				q.Plan = ptr("Go")
			}
		}
		for _, item := range []struct {
			key  string
			span uint64
		}{{"rolling", 18000}, {"weekly", 604800}, {"monthly", 0}} {
			meter := obj(usage[item.key])
			used := percent(meter["percent"])
			if used != nil && (provider == "opencode" || *used > 0) {
				q.Windows = append(q.Windows, window(item.key, item.key, used, timestamp(meter["resetsAt"]), item.span))
			}
		}
	case "deepseek":
		for _, row := range list(body["balance_infos"]) {
			unit := text(at(row, "currency"))
			if amount := money(at(row, "total_balance")); amount != nil && (unit == "CNY" || unit == "USD") {
				q.Balances = append(q.Balances, Balance{Unit: unit, Remaining: *amount})
			}
		}
	case "stepfun", "stepfun-ai", "stepfun-ai-step-plan", "stepfun-step-plan":
		if text(body["object"]) == "account" {
			if amount := money(body["balance"]); amount != nil {
				q.Balances = append(q.Balances, Balance{Remaining: *amount})
			}
		}
	case "openrouter":
		credits, used := number(at(body, "data", "total_credits")), number(at(body, "data", "total_usage"))
		if credits != nil && used != nil {
			q.Balances = append(q.Balances, Balance{Unit: "USD", Remaining: fmt.Sprintf("%.2f", *credits-*used)})
		}
	case "siliconflow", "siliconflow-cn", "moonshotai", "moonshotai-cn":
		key := "totalBalance"
		if strings.HasPrefix(provider, "moonshot") {
			key = "available_balance"
		}
		unit := "USD"
		if strings.HasSuffix(provider, "-cn") {
			unit = "CNY"
		}
		if amount := number(at(body, "data", key)); amount != nil {
			q.Balances = append(q.Balances, Balance{Unit: unit, Remaining: fmt.Sprintf("%.2f", *amount)})
		}
	case "zhipuai", "zai":
		code := number(body["code"])
		failed := body["success"] == false || body["error"] != nil || code != nil && *code != 200
		if failed {
			q.Status = "unavailable"
			if code == nil {
				code = number(at(body, "error", "code"))
			}
			if code != nil && *code >= 1000 && *code <= 1005 {
				q.Status = "needs_auth"
			}
		} else if amount := money(at(body, "data", "availableBalance")); amount != nil {
			unit := "CNY"
			if provider == "zai" {
				unit = "USD"
			}
			q.Balances = append(q.Balances, Balance{Unit: unit, Remaining: *amount})
		}
	case "zhipuai-coding-plan", "zai-coding-plan":
		if body["success"] == false {
			return empty("no_data")
		}
		if plan := text(at(body, "data", "level")); plan != "" {
			q.Plan = &plan
		}
		for index, row := range list(at(body, "data", "limits")) {
			used := percent(at(row, "percentage"))
			if used == nil {
				continue
			}
			kind := ""
			var span uint64
			switch text(at(row, "type")) {
			case "TIME_LIMIT":
				kind = "tool_calls"
			case "TOKENS_LIMIT", "CREDIT_LIMIT":
				unit := number(at(row, "unit"))
				if unit != nil {
					if *unit == 6 {
						kind = "weekly"
						span = 604800
					} else if *unit == 3 {
						kind = "rolling"
						hours := number(at(row, "number"))
						span = 18000
						if hours != nil {
							span = uint64(math.Max(1, *hours)) * 3600
						}
					}
				}
			}
			if kind != "" {
				q.Windows = append(q.Windows, window(fmt.Sprintf("limit-%d", index), kind, used, timestamp(at(row, "nextResetTime")), span))
			}
		}
	case "google":
		for _, row := range list(body["buckets"]) {
			model, left := text(at(row, "modelId")), number(at(row, "remainingFraction"))
			if model == "" || left == nil || *left < 0 || *left > 1 {
				continue
			}
			w := window(model+":"+text(at(row, "tokenType")), "model", ptr((1-*left)*100), timestamp(at(row, "resetTime")), 0)
			w.Model = &model
			w.Remaining = money(at(row, "remainingAmount"))
			q.Windows = append(q.Windows, w)
		}
	case "github-copilot":
		parseCopilot(body, &q)
	case "minimax", "minimax-cn", "minimax-coding-plan", "minimax-cn-coding-plan":
		parseMinimax(body, &q)
	case "kimi-code-plan-cn", "kimi-code-plan-global":
		parseKimi(body, &q)
	case "commandcode":
		parseCommandcode(body, &q)
	case "ollama-cloud":
		parseOllama(body, &q)
	default:
		return empty("unsupported")
	}
	if q.Status == "ready" && len(q.Windows) == 0 && len(q.Balances) == 0 {
		q.Status = "no_data"
	}
	return q
}

func parseCopilot(body object, q *Quota) {
	if plan := text(body["copilot_plan"]); plan != "" {
		q.Plan = &plan
	}
	reset := timestamp(body["quota_reset_date_utc"])
	if reset == nil {
		if t, err := time.Parse("2006-01-02", text(body["quota_reset_date"])); err == nil {
			reset = ptr(t.Unix())
		}
	}
	for _, item := range []struct{ id, kind string }{{"chat", "chat"}, {"completions", "completions"}, {"premium_interactions", "premium"}} {
		row := obj(at(body, "quota_snapshots", item.id))
		if len(row) == 0 {
			continue
		}
		date := timestamp(row["quota_reset_at"])
		if date == nil {
			date = reset
		}
		w := window(item.id, item.kind, nil, date, 0)
		if row["unlimited"] == true {
			if item.kind == "premium" && row["has_quota"] == false {
				w.UsedPercent = ptr(100.0)
			} else {
				w.Unlimited = true
			}
		} else {
			if total := number(row["entitlement"]); total != nil && *total <= 0 {
				continue
			}
			addCount(&w, row["entitlement"], nil, row["quota_remaining"])
			if w.UsedPercent == nil {
				if left := percent(row["percent_remaining"]); left != nil {
					w.UsedPercent = ptr(100 - *left)
				}
			}
		}
		if w.Unlimited || w.UsedPercent != nil {
			q.Windows = append(q.Windows, w)
		}
	}
}

func parseMinimax(body object, q *Quota) {
	payload := obj(body["data"])
	if len(payload) == 0 {
		payload = body
	}
	verdict := obj(payload["base_resp"])
	if len(verdict) == 0 {
		verdict = obj(body["base_resp"])
	}
	if len(verdict) == 0 {
		q.Status = "unavailable"
		return
	}
	if code := number(verdict["status_code"]); code != nil && *code != 0 {
		q.Status = "unavailable"
		if *code == 1004 {
			q.Status = "needs_auth"
		}
		return
	}
	for _, key := range []string{"current_subscribe_title", "plan_name", "combo_title", "current_plan_title"} {
		if plan := strings.TrimSpace(text(payload[key])); plan != "" {
			q.Plan = &plan
			break
		}
	}
	for _, row := range list(payload["model_remains"]) {
		model := strings.TrimSpace(text(at(row, "model_name")))
		if model == "" {
			continue
		}
		for _, item := range []struct{ id, start, end, left, state string }{{"rolling", "start_time", "end_time", "current_interval_remaining_percent", "current_interval_status"}, {"weekly", "weekly_start_time", "weekly_end_time", "current_weekly_remaining_percent", "current_weekly_status"}} {
			state := number(at(row, item.state))
			if state != nil && *state == 3 {
				continue
			}
			var used *float64
			if state != nil && *state == 2 {
				used = ptr(100.0)
			} else if left := number(at(row, item.left)); left != nil {
				used = ptr(math.Max(0, math.Min(100, 100-*left)))
			}
			if used == nil {
				continue
			}
			reset, start := timestamp(at(row, item.end)), timestamp(at(row, item.start))
			var span uint64
			if item.id == "weekly" {
				span = 604800
			}
			if reset != nil && start != nil && *reset > *start {
				span = uint64(*reset - *start)
			}
			w := window(model+":"+item.id, item.id, used, reset, span)
			if model != "general" {
				w.Model = &model
			}
			q.Windows = append(q.Windows, w)
		}
	}
}
