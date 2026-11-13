package backendengine

import (
	"context"
	"encoding/json"
	"net/url"

	"fintwind-go/internal/model"
)

type tokenUsage struct {
	Input     uint64 `json:"input"`
	Output    uint64 `json:"output"`
	Reasoning uint64 `json:"reasoning"`
	Total     uint64 `json:"total"`
	Cache     struct {
		Read  uint64 `json:"read"`
		Write uint64 `json:"write"`
	} `json:"cache"`
}

func (u tokenUsage) prompt() uint64 { return u.Input + u.Cache.Read + u.Cache.Write }
func (u tokenUsage) context() uint64 {
	if u.Total > 0 {
		return u.Total
	}
	return u.prompt() + u.Output + u.Reasoning
}

func (c *Client) NativeUsage(ctx context.Context, session string) (*model.ContextUsage, error) {
	path := "/api/session/" + url.PathEscape(session)
	raw, err := c.call(ctx, "GET", path, nil)
	if err != nil {
		return nil, err
	}
	var info struct {
		Data struct {
			Tokens     tokenUsage `json:"tokens"`
			Input      uint64     `json:"tokens_input"`
			Output     uint64     `json:"tokens_output"`
			Reasoning  uint64     `json:"tokens_reasoning"`
			CacheRead  uint64     `json:"tokens_cache_read"`
			CacheWrite uint64     `json:"tokens_cache_write"`
		} `json:"data"`
	}
	if err := json.Unmarshal(raw, &info); err != nil {
		return nil, err
	}
	totals := info.Data.Tokens
	if totals.context() == 0 {
		totals.Input, totals.Output, totals.Reasoning = info.Data.Input, info.Data.Output, info.Data.Reasoning
		totals.Cache.Read, totals.Cache.Write = info.Data.CacheRead, info.Data.CacheWrite
	}
	result := &model.ContextUsage{}
	if total := totals.context(); total > 0 {
		prompt, cache := totals.prompt(), totals.Cache.Read
		result.TotalTokens, result.PromptTokens, result.CacheRead = &total, &prompt, &cache
	}
	raw, err = c.call(ctx, "GET", path+"/message?limit=5", nil)
	if err != nil {
		return nil, err
	}
	var messages struct {
		Data []struct {
			Type   string     `json:"type"`
			Role   string     `json:"role"`
			Tokens tokenUsage `json:"tokens"`
		} `json:"data"`
	}
	if err := json.Unmarshal(raw, &messages); err != nil {
		return nil, err
	}
	for _, message := range messages.Data {
		if message.Type == "assistant" || message.Role == "assistant" {
			if value := message.Tokens.context(); value > 0 {
				result.Tokens, result.Measured = value, true
				break
			}
		}
	}
	return result, nil
}
