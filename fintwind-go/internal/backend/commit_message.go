package backend

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"unicode/utf8"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/backendworkspace"
)

func (s *Server) generateCommitMessage(ctx context.Context, op backendstore.Object) (backendstore.Object, bool, error) {
	invocation, err := backendstore.Decode(op["invocation"])
	if err != nil || invocation.String("binary") == "" {
		return nil, true, errors.New("commit message agent invocation required")
	}
	prompt, err := backendworkspace.CommitPrompt(ctx, s.workspaceForPath(op.String("cwd")), op.String("cwd"), op.Bool("includeUnstaged"))
	if err != nil {
		return nil, true, err
	}
	api, err := s.servicePool.acquire(ctx, backendstore.Fields(map[string]any{"binary": invocation.String("binary"), "directory": op.String("cwd")}))
	if err != nil {
		return nil, true, err
	}
	body := map[string]any{"prompt": prompt}
	model := invocation.String("model")
	effort := invocation.String("reasoning_effort")
	if effort == "" {
		effort = invocation.String("reasoningEffort")
	}
	if model == "" && effort != "" {
		raw, e := api.RoundTrip(ctx, "GET", "/api/model/default", nil)
		if e != nil {
			return nil, true, e
		}
		data, e := registryData(raw)
		if e != nil {
			return nil, true, e
		}
		selected, e := backendstore.Decode(data)
		if e != nil || selected.String("providerID") == "" || selected.String("id") == "" {
			return nil, true, errors.New("default commit message model unavailable")
		}
		model = selected.String("providerID") + "/" + selected.String("id")
	}
	if model != "" {
		provider, id, ok := strings.Cut(model, "/")
		if !ok || provider == "" || id == "" {
			return nil, true, errors.New("invalid commit message model")
		}
		ref := map[string]string{"providerID": provider, "id": id}
		if effort != "" {
			ref["variant"] = effort
		}
		body["model"] = ref
	}
	raw, err := api.RoundTrip(ctx, "POST", "/api/experimental/generate/text", body)
	if err != nil {
		return nil, true, err
	}
	var response struct {
		Data struct {
			Text *string `json:"text"`
		} `json:"data"`
	}
	if len(raw) > 1<<20 || json.Unmarshal(raw, &response) != nil || response.Data.Text == nil {
		return nil, true, errors.New("invalid commit message response")
	}
	text := normalizeCommitMessage(*response.Data.Text)
	if text == "" {
		return nil, true, errors.New("agent returned no commit message")
	}
	return backendstore.Reply("workspace", backendstore.Fields(map[string]any{"result": map[string]any{"type": "commitMessage", "message": text}})), true, nil
}

func normalizeCommitMessage(text string) string {
	var clean strings.Builder
	escape := false
	for _, r := range text {
		if escape {
			if r >= '@' && r <= '~' && r != '[' {
				escape = false
			}
			continue
		}
		if r == 27 {
			escape = true
			continue
		}
		clean.WriteRune(r)
	}
	candidate := ""
	for _, line := range strings.Split(clean.String(), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || line == "```" || strings.HasPrefix(line, "[tool]") || strings.HasPrefix(line, "[thinking]") {
			continue
		}
		candidate = line
	}
	candidate = strings.TrimSpace(strings.Trim(candidate, "`"))
	for _, prefix := range []string{"Commit message:", "Commit subject:"} {
		candidate = strings.TrimPrefix(candidate, prefix)
	}
	candidate = strings.TrimSpace(strings.Trim(strings.TrimSpace(candidate), "\"'`"))
	candidate = strings.TrimSpace(strings.TrimRight(candidate, "."))
	if utf8.RuneCountInString(candidate) > 200 {
		candidate = string([]rune(candidate)[:200])
	}
	return candidate
}
