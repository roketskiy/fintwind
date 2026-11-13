package backendengine

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"
)

type PromptSkill struct {
	ID string `json:"id"`
}

// Resolve at dispatch, not from the UI's cached command list. IDs are exact and
// case-sensitive; display names must never stand in for native skill IDs.
func (c *Client) PromptSkills(ctx context.Context, text string) ([]PromptSkill, error) {
	if !strings.Contains(text, "@") && !strings.HasPrefix(text, "/") && !strings.Contains(text, "\n/") {
		return nil, nil
	}
	work, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	query := "?location%5Bdirectory%5D=" + url.QueryEscape(c.directory)
	if _, err := c.call(work, "GET", "/api/config"+query, nil); err != nil {
		return nil, err
	}
	raw, err := c.call(work, "GET", "/api/skill"+query, nil)
	if err != nil {
		return nil, err
	}
	var envelope struct {
		Data json.RawMessage `json:"data"`
	}
	if json.Unmarshal(raw, &envelope) != nil || len(envelope.Data) == 0 || string(envelope.Data) == "null" {
		return nil, errors.New("invalid native skill registry")
	}
	var registry []PromptSkill
	if json.Unmarshal(envelope.Data, &registry) != nil || len(registry) > 10000 {
		return nil, errors.New("invalid native skill registry")
	}
	for _, skill := range registry {
		if skill.ID == "" || len(skill.ID) > 1024 {
			return nil, errors.New("invalid native skill identity")
		}
	}
	seen := map[string]bool{}
	attachments := []PromptSkill{}
	for offset, sigil := range text {
		if sigil != '@' && (sigil != '/' || (offset != 0 && text[offset-1] != '\n')) {
			continue
		}
		rest := text[offset+1:]
		matched := ""
		for _, skill := range registry {
			tail, ok := strings.CutPrefix(rest, skill.ID)
			if !ok {
				continue
			}
			if tail != "" {
				next, _ := utf8.DecodeRuneInString(tail)
				if !unicode.IsSpace(next) && !strings.ContainsRune(",;:!?)]}，。！？；：", next) {
					continue
				}
			}
			if len(skill.ID) > len(matched) {
				matched = skill.ID
			}
		}
		if matched != "" && !seen[matched] {
			attachments = append(attachments, PromptSkill{matched})
			seen[matched] = true
		}
	}
	return attachments, nil
}
