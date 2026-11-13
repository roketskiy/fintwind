package backendservices

import (
	"context"
	"encoding/json"
	"errors"
	"math"
	"net/url"
	"strings"

	"fintwind-go/internal/backendstore"
)

func saturated(a, b uint64) uint64 {
	if math.MaxUint64-a < b {
		return math.MaxUint64
	}
	return a + b
}
func rawUint(value json.RawMessage) (uint64, bool) {
	var n uint64
	if len(value) == 0 || string(value) == "null" || json.Unmarshal(value, &n) != nil {
		return 0, false
	}
	return n, true
}
func fetchTurnStats(ctx context.Context, api API, c backendstore.Object) (backendstore.Object, error) {
	none := func() (backendstore.Object, error) {
		return backendstore.Reply("nativeTurnStats", backendstore.Fields(map[string]any{"stats": nil})), nil
	}
	var ids []string
	if err := json.Unmarshal(c["stepIds"], &ids); err != nil {
		return nil, err
	}
	if len(ids) == 0 || len(ids) > 256 {
		return none()
	}
	remaining := map[string]bool{}
	for _, id := range ids {
		if id == "" || remaining[id] {
			return none()
		}
		remaining[id] = true
	}
	session := c.String("sessionId")
	if session == "" {
		return nil, errors.New("native session ID required")
	}
	found := map[string]backendstore.Object{}
	cursor := ""
	seen := map[string]bool{}
	for page := 0; page < 10; page++ {
		path := "/api/session/" + url.PathEscape(session) + "/message?limit=200"
		if cursor != "" {
			path += "&cursor=" + url.QueryEscape(cursor)
		}
		raw, err := api.RoundTrip(ctx, "GET", path, nil)
		if err != nil {
			return nil, err
		}
		var response struct {
			Data   []backendstore.Object `json:"data"`
			Cursor struct {
				Next string `json:"next"`
			} `json:"cursor"`
		}
		if err := json.Unmarshal(raw, &response); err != nil {
			return nil, err
		}
		if response.Data == nil {
			return none()
		}
		for _, row := range response.Data {
			id := row.String("id")
			if remaining[id] {
				delete(remaining, id)
				found[id] = row
			}
		}
		if len(remaining) == 0 {
			break
		}
		if response.Cursor.Next == "" || len(response.Data) == 0 || seen[response.Cursor.Next] {
			break
		}
		cursor = response.Cursor.Next
		seen[cursor] = true
	}
	if len(remaining) > 0 {
		return none()
	}
	var output, stream uint64
	var model, agent any
	for _, id := range ids {
		row := found[id]
		if row.String("type") != "assistant" {
			return none()
		}
		timing, err := backendstore.Decode(row["time"])
		if err != nil {
			return none()
		}
		created, hasCreated := rawUint(timing["created"])
		streamed, hasStreamed := rawUint(timing["streamed"])
		if !hasCreated || !hasStreamed {
			return none()
		}
		var duration uint64
		if streamed > created {
			duration = streamed - created
		}
		tokens, _ := backendstore.Decode(row["tokens"])
		textTokens, _ := rawUint(tokens["output"])
		reasoning, _ := rawUint(tokens["reasoning"])
		stepOutput := saturated(textTokens, reasoning)
		stepModel := ""
		if choice, err := backendstore.Decode(row["model"]); err == nil && choice.String("providerID") != "" && choice.String("id") != "" {
			stepModel = choice.String("providerID") + "/" + choice.String("id")
		}
		stepAgent := row.String("agent")
		if stepModel == "" && strings.TrimSpace(stepAgent) == "" && stepOutput == 0 && duration == 0 {
			return none()
		}
		if stepModel != "" {
			model = stepModel
		}
		if strings.TrimSpace(stepAgent) != "" {
			agent = stepAgent
		}
		output = saturated(output, stepOutput)
		stream = saturated(stream, duration)
	}
	stats := map[string]any{"output_tokens": output, "stream_ms": stream, "stream_verified": true}
	if model != nil {
		stats["model"] = model
	}
	if agent != nil {
		stats["agent"] = agent
	}
	return backendstore.Reply("nativeTurnStats", backendstore.Fields(map[string]any{"stats": stats})), nil
}
