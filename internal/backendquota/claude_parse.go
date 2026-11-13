package backendquota

import (
	"regexp"
	"strconv"
	"strings"
	"time"
	_ "time/tzdata"
)

var claudeDenied = regexp.MustCompile(`(?i)\b(401|403)\b|not (logged|signed) in|signed out|sign-in (has )?expired|unauthorized|forbidden|authentication (failed|required)|invalid (access )?token|(session|usage) limit|hit your limit`)
var claudeWindow = regexp.MustCompile(`^(session|week(?: \(([^)]*)\))?)`)
var claudeUsed = regexp.MustCompile(`([0-9.]+)%\s+used`)
var claudeReset = regexp.MustCompile(`(?i)resets\s+(.+?\))`)

func parseClaudePanel(text string, now int64) Quota {
	if claudeDenied.MatchString(text) {
		return empty("needs_auth")
	}
	q := empty("ready")
	flat := strings.Join(strings.Fields(text), " ")
	seen := map[string]int{}
	for _, chunk := range strings.Split(flat, "Current ")[1:] {
		name := claudeWindow.FindStringSubmatch(chunk)
		usedMatch := claudeUsed.FindStringSubmatch(chunk)
		if len(name) == 0 || len(usedMatch) == 0 {
			continue
		}
		used, err := strconv.ParseFloat(usedMatch[1], 64)
		if err != nil || used < 0 || used > 100 {
			continue
		}
		id, kind, span := "session", "rolling", uint64(5*3600)
		var scope *string
		if strings.HasPrefix(name[1], "week") {
			id = "weekly"
			kind = "weekly"
			span = 7 * 86400
			if len(name) > 2 {
				label := strings.TrimSpace(name[2])
				if strings.HasSuffix(strings.ToLower(label), " only") {
					label = strings.TrimSpace(label[:len(label)-5])
				}
				if label != "" && !strings.EqualFold(label, "all models") {
					scope = &label
					id = "week:" + strings.ToLower(label)
				}
			}
		}
		var resets *int64
		if reset := claudeReset.FindStringSubmatch(chunk); len(reset) > 1 {
			resets = claudeResetTime(reset[1], now, span)
		}
		w := window(id, kind, &used, resets, span)
		w.Model = scope
		if index, ok := seen[id]; ok {
			q.Windows[index] = w
		} else {
			seen[id] = len(q.Windows)
			q.Windows = append(q.Windows, w)
		}
	}
	if strings.Contains(strings.ToLower(flat), "using your overages") {
		used := 100.0
		extra := window("extra-usage", "extraUsage", &used, nil, 0)
		for _, w := range q.Windows {
			if w.UsedPercent != nil && *w.UsedPercent >= 100 && w.ResetsAt != nil && (extra.ResetsAt == nil || *w.ResetsAt > *extra.ResetsAt) {
				extra.ResetsAt = w.ResetsAt
			}
		}
		q.Windows = append(q.Windows, extra)
	}
	if len(q.Windows) == 0 {
		return empty("unavailable")
	}
	return q
}

func claudeResetTime(text string, now int64, span uint64) *int64 {
	zone := time.Local
	stamp := strings.TrimSpace(text)
	if open := strings.LastIndex(stamp, "("); open >= 0 && strings.HasSuffix(stamp, ")") {
		if parsed, err := time.LoadLocation(strings.TrimSpace(stamp[open+1 : len(stamp)-1])); err == nil {
			zone = parsed
		}
		stamp = strings.TrimSpace(stamp[:open])
	}
	stamp = strings.ReplaceAll(stamp, " at ", " ")
	stamp = strings.ReplaceAll(stamp, ",", "")
	stamp = strings.ReplaceAll(stamp, "AM", "am")
	stamp = strings.ReplaceAll(stamp, "PM", "pm")
	current := time.Unix(now, 0).In(zone)
	for _, layout := range []string{"Jan 2 2006 3:04pm", "Jan 2 2006 3pm", "Jan 2 3:04pm", "Jan 2 3pm", "January 2 3:04pm", "January 2 3pm", "3:04pm", "3pm"} {
		parsed, err := time.ParseInLocation(layout, stamp, zone)
		if err != nil {
			continue
		}
		if strings.Contains(layout, "2006") {
			value := parsed.Unix()
			return &value
		}
		year, month, day := current.Date()
		candidates := []time.Time{}
		if strings.Contains(layout, "Jan") {
			for y := year - 1; y <= year+1; y++ {
				candidate := time.Date(y, parsed.Month(), parsed.Day(), parsed.Hour(), parsed.Minute(), 0, 0, zone)
				if candidate.Month() == parsed.Month() && candidate.Day() == parsed.Day() {
					candidates = append(candidates, candidate)
				}
			}
		} else {
			for d := 0; d <= 2; d++ {
				candidates = append(candidates, time.Date(year, month, day+d, parsed.Hour(), parsed.Minute(), 0, 0, zone))
			}
		}
		latest := now + int64(span) + 2*3600
		var past *int64
		for _, candidate := range candidates {
			for _, delta := range []time.Duration{0, time.Hour, -time.Hour} {
				at := candidate.Add(delta)
				if at.Hour() != parsed.Hour() || at.Minute() != parsed.Minute() {
					continue
				}
				value := at.Unix()
				if value >= now && value <= latest {
					return &value
				}
				if value < now && value >= now-86400 && (past == nil || value > *past) {
					copy := value
					past = &copy
				}
			}
		}
		return past
	}
	return nil
}
