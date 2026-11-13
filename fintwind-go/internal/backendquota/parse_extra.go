package backendquota

import (
	"fmt"
	"math"
	"sort"
	"strings"
)

func parseCommandcode(body object, q *Quota) {
	credits := obj(body["credits"])
	if plan := text(credits["planId"]); plan != "" {
		normalized := strings.ReplaceAll(strings.ToLower(plan), "_", "-")
		for _, item := range []struct{ prefix, label string }{{"individual-provider", "Provider"}, {"individual-pro-v1", "Pro"}, {"individual-goat", "GOAT"}, {"individual-ultra", "Ultra"}, {"individual-max", "Max"}, {"individual-pro", "Pro"}, {"individual-go", "Go"}, {"teams-pro", "Teams Pro"}} {
			if strings.HasPrefix(normalized, item.prefix) {
				plan = item.label
				break
			}
		}
		q.Plan = &plan
	}
	for _, item := range []struct {
		key, kind string
		span      uint64
	}{{"fiveHour", "rolling", 18000}, {"weekly", "weekly", 604800}} {
		row := obj(at(body, "windowLimits", item.key))
		used, cap := number(row["used"]), number(row["cap"])
		if used != nil && cap != nil && *cap > 0 {
			q.Windows = append(q.Windows, window(item.key, item.kind, ptr(math.Max(0, math.Min(100, *used / *cap * 100))), timestamp(row["resetAt"]), item.span))
		}
	}
	left, found := 0.0, false
	for _, key := range []string{"monthlyCredits", "purchasedCredits", "freeCredits"} {
		if n := number(credits[key]); n != nil {
			left += math.Max(0, *n)
			found = true
		}
	}
	if found {
		q.Balances = append(q.Balances, Balance{Unit: "USD", Remaining: fmt.Sprintf("%.2f", left)})
	}
}

func parseOllama(body object, q *Quota) {
	included := obj(body["included"])
	for _, item := range []struct {
		key, kind string
		span      uint64
	}{{"session", "rolling", 0}, {"weekly", "weekly", 604800}} {
		if left := number(at(included, item.key, "remaining_percent")); left != nil {
			q.Windows = append(q.Windows, window(item.key, item.kind, ptr(math.Max(0, math.Min(100, 100-*left))), timestamp(at(included, item.key, "resets_at")), item.span))
		}
	}
	allowance, left := number(included["allowance_usd"]), number(included["balance_usd"])
	if allowance != nil && left != nil && *allowance > 0 {
		w := window("allowance", "rolling", ptr(math.Max(0, math.Min(*allowance, *allowance-*left)) / *allowance * 100), timestamp(at(included, "period", "until")), 0)
		from, until := timestamp(at(included, "period", "from")), w.ResetsAt
		if from != nil && until != nil && *until > *from {
			w.SpanSeconds = ptr(uint64(*until - *from))
			days := float64(*until-*from) / 86400
			if days >= 27 && days <= 32 {
				w.ID = "month"
				w.Kind = "monthly"
			}
		}
		w.Remaining = ptr(fmt.Sprintf("%.2f", math.Max(0, *left)))
		w.Limit = ptr(fmt.Sprintf("%.2f", *allowance))
		q.Windows = append(q.Windows, w)
	}
	if purchased := number(at(body, "purchased", "balance_usd")); purchased != nil {
		q.Balances = append(q.Balances, Balance{Unit: "USD", Remaining: fmt.Sprintf("%.2f", *purchased)})
	}
}

func kimiReset(value any) *int64 {
	for _, key := range []string{"resetTime", "resetAt", "reset_time", "reset_at"} {
		raw := at(value, key)
		if n := timestamp(raw); n != nil {
			return n
		}
		if n := number(raw); n != nil && *n > 0 {
			stamp := *n
			if stamp > 1e11 {
				stamp /= 1000
			}
			return ptr(int64(stamp))
		}
	}
	return nil
}
func kimiCounts(id, kind string, span uint64, value any) Window {
	w := window(id, kind, nil, kimiReset(value), span)
	addCount(&w, at(value, "limit"), at(value, "used"), at(value, "remaining"))
	return w
}
func parseKimi(body object, q *Quota) {
	counts := []Window{}
	for index, row := range list(body["limits"]) {
		duration := number(at(row, "window", "duration"))
		unit := text(at(row, "window", "timeUnit"))
		var seconds float64
		for _, item := range []struct {
			key string
			n   float64
		}{{"MINUTE", 60}, {"HOUR", 3600}, {"DAY", 86400}, {"WEEK", 604800}, {"MONTH", 2592000}} {
			if strings.Contains(unit, item.key) {
				seconds = item.n
				break
			}
		}
		var span uint64
		if duration != nil && *duration*seconds > 0 {
			span = uint64(*duration * seconds)
		}
		kind := "rolling"
		if span == 604800 {
			kind = "weekly"
		} else if span == 2592000 {
			kind = "monthly"
		}
		value := obj(at(row, "detail"))
		if len(value) == 0 {
			value = obj(at(row, "window"))
		}
		w := kimiCounts(fmt.Sprintf("limit-%d", index), kind, span, value)
		if w.UsedPercent != nil {
			counts = append(counts, w)
		}
	}
	if w := kimiCounts("weekly", "weekly", 604800, body["usage"]); w.UsedPercent != nil {
		counts = append(counts, w)
	}
	shares := []Window{}
	spans := map[uint64]bool{}
	for _, item := range []struct {
		id, camel, kind string
		span            uint64
	}{{"limit_5h", "limit5h", "rolling", 18000}, {"limit_7d", "limit7d", "weekly", 604800}, {"limit_month_total", "limitMonthTotal", "monthly", 2592000}, {"limit_month_code", "limitMonthCode", "monthly", 2592000}} {
		pool := obj(at(body, "usages", item.id))
		if len(pool) == 0 {
			pool = obj(at(body, "usages", item.camel))
		}
		ratio := number(pool["used_ratio"])
		if ratio == nil {
			ratio = number(pool["usedRatio"])
		}
		if ratio == nil || spans[item.span] {
			continue
		}
		spans[item.span] = true
		shares = append(shares, window(item.id, item.kind, ptr(math.Max(0, math.Min(100, *ratio*100))), kimiReset(pool), item.span))
	}
	if len(shares) > 0 {
		filtered := []Window{}
		for _, w := range counts {
			if w.SpanSeconds == nil || !spans[*w.SpanSeconds] {
				filtered = append(filtered, w)
			}
		}
		counts = append(filtered, shares...)
		sort.SliceStable(counts, func(i, j int) bool {
			a, b := ^uint64(0), ^uint64(0)
			if counts[i].SpanSeconds != nil {
				a = *counts[i].SpanSeconds
			}
			if counts[j].SpanSeconds != nil {
				b = *counts[j].SpanSeconds
			}
			return a < b
		})
	}
	if len(counts) == 0 {
		q.Status = "unavailable"
	}
	q.Windows = append(q.Windows, counts...)
}
