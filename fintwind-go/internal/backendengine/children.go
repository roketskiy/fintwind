package backendengine

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"

	"fintwind-go/internal/backendstore"
)

// Children returns a complete roster and an active level signal from the same
// directory. A partial/malformed listing is never an authoritative empty list.
func (c *Client) Children(ctx context.Context, parent string) ([]backendstore.Object, map[string]json.RawMessage, error) {
	var rows []backendstore.Object
	cursor := ""
	seen := map[string]bool{}
	filter := true
	for page := 0; page < 50; page++ {
		query := url.Values{"directory": {c.directory}, "limit": {"200"}}
		if cursor != "" {
			query.Set("cursor", cursor)
		}
		if filter {
			query.Set("parentID", parent)
		}
		raw, err := c.call(ctx, "GET", "/api/session?"+query.Encode(), nil)
		var status *HTTPError
		if errors.As(err, &status) && status.Status == 400 && filter {
			filter = false
			query.Del("parentID")
			raw, err = c.call(ctx, "GET", "/api/session?"+query.Encode(), nil)
		}
		if err != nil {
			return nil, nil, err
		}
		var response struct {
			Data   json.RawMessage `json:"data"`
			Cursor json.RawMessage `json:"cursor"`
		}
		if json.Unmarshal(raw, &response) != nil || len(response.Data) == 0 || response.Data[0] != '[' {
			return nil, nil, errors.New("invalid child session roster")
		}
		var batch []backendstore.Object
		if json.Unmarshal(response.Data, &batch) != nil || len(batch) > 200 {
			return nil, nil, errors.New("invalid child session roster")
		}
		for _, row := range batch {
			p := row.String("parentID")
			if p == "" {
				p = row.String("parentId")
			}
			if p == parent {
				rows = append(rows, row)
			}
		}
		if len(rows) > 256 {
			return nil, nil, errors.New("child session roster exceeds budget")
		}
		if len(batch) == 200 && len(response.Cursor) == 0 {
			return nil, nil, errors.New("child session roster is incomplete")
		}
		var next struct {
			Next *string `json:"next"`
		}
		if len(response.Cursor) > 0 && string(response.Cursor) != "null" && json.Unmarshal(response.Cursor, &next) != nil {
			return nil, nil, errors.New("invalid child session cursor")
		}
		if next.Next == nil || *next.Next == "" {
			activeRaw, err := c.call(ctx, "GET", "/api/session/active", nil)
			if err != nil {
				return nil, nil, err
			}
			var active struct {
				Data json.RawMessage `json:"data"`
			}
			if json.Unmarshal(activeRaw, &active) != nil || len(active.Data) == 0 || active.Data[0] != '{' {
				return nil, nil, errors.New("invalid active child session roster")
			}
			var values map[string]json.RawMessage
			if json.Unmarshal(active.Data, &values) != nil || values == nil {
				return nil, nil, errors.New("invalid active child session roster")
			}
			return rows, values, nil
		}
		if len(batch) == 0 || seen[*next.Next] {
			return nil, nil, errors.New("child session cursor did not advance")
		}
		cursor = *next.Next
		seen[cursor] = true
	}
	return nil, nil, errors.New("child session roster is incomplete")
}
