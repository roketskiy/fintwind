package backendquota

import (
	"context"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// PlanUsage is the retained legacy command. It reports only OpenCode Go,
// never substitutes Console/Zen or an unrelated provider's credential.
func (s *Service) PlanUsage(ctx context.Context, home string) (any, error) {
	var auth object
	if s.environment != nil {
		if raw, ok := s.environment("OPENCODE_AUTH_CONTENT"); ok {
			auth, _ = decode([]byte(raw))
		}
	}
	if auth == nil {
		root, err := openQuotaHome(home)
		if err != nil {
			return nil, err
		}
		defer root.Close()
		file, err := root.Open(filepath.Join("opencode", "auth.json"))
		if err == nil {
			raw, e := io.ReadAll(io.LimitReader(file, (1<<20)+1))
			file.Close()
			if e != nil || len(raw) > 1<<20 {
				return nil, errors.New("plan usage credential file unreadable")
			}
			auth, _ = decode(raw)
		} else if !errors.Is(err, os.ErrNotExist) {
			return nil, errors.New("plan usage credential file unavailable")
		}
	}
	key := ""
	if entry := obj(auth["opencode-go"]); text(entry["type"]) == "api" {
		key = strings.TrimSpace(text(entry["key"]))
	}
	if key == "" && s.environment != nil {
		key, _ = s.environment("OPENCODE_API_KEY")
		key = strings.TrimSpace(key)
	}
	if key == "" {
		return nil, nil
	}
	for _, r := range key {
		if r < 32 || r == 127 || r == '"' || r == '\\' {
			return nil, errors.New("invalid OpenCode Go credential")
		}
	}
	deadline, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	request, err := http.NewRequestWithContext(deadline, "GET", endpoints["opencode-go"], nil)
	if err != nil {
		return nil, err
	}
	request.Header.Set("Authorization", "Bearer "+key)
	request.Header.Set("Accept", "application/json")
	request.Header.Set("User-Agent", "fintwind")
	response, err := s.http.Do(request)
	if err != nil {
		return nil, errors.New("OpenCode Go plan usage unavailable")
	}
	defer response.Body.Close()
	if response.StatusCode != 200 {
		return nil, errors.New("OpenCode Go plan usage request refused")
	}
	raw, err := io.ReadAll(io.LimitReader(response.Body, (8<<20)+1))
	if err != nil || len(raw) > 8<<20 {
		return nil, errors.New("OpenCode Go plan response exceeds budget")
	}
	body, err := decode(raw)
	if err != nil {
		return nil, errors.New("invalid OpenCode Go plan response")
	}
	windows := []map[string]any{}
	for _, item := range []struct{ key, label string }{{"rolling", "5-hour limit"}, {"weekly", "Weekly limit"}, {"monthly", "Monthly limit"}} {
		row := obj(at(body, "usage", item.key))
		used := percent(row["percent"])
		if used == nil {
			continue
		}
		windows = append(windows, map[string]any{"label": item.label, "percent": *used, "resetsAt": timestamp(row["resetsAt"])})
	}
	if len(windows) == 0 {
		return nil, errors.New("OpenCode Go plan response has no windows")
	}
	return map[string]any{"planLabel": "Go", "windows": windows}, nil
}
