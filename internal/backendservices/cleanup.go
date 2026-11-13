package backendservices

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendstore"
)

// CleanupCatalog is a bounded, validated provider inventory. It is not a
// deletion authority: every local task and every live native node is checked
// again by the caller immediately before the single recursive delete.
type CleanupCatalog struct {
	Rows   map[string]cleanupNative
	Active map[string]json.RawMessage
}
type cleanupNative struct {
	ID          string `json:"id"`
	Parent      string `json:"parentID"`
	ParentAlias string `json:"parentId"`
	Time        struct {
		Created uint64 `json:"created"`
		Updated uint64 `json:"updated"`
	} `json:"time"`
}

func ReadCleanupCatalog(ctx context.Context, api API) (*CleanupCatalog, error) {
	inventory := &CleanupCatalog{Rows: map[string]cleanupNative{}}
	cursor := ""
	seen := map[string]bool{}
	for page := 0; page < 50; page++ {
		path := "/api/session?limit=500"
		if cursor != "" {
			path += "&cursor=" + url.QueryEscape(cursor)
		}
		raw, err := api.RoundTrip(ctx, "GET", path, nil)
		if err != nil {
			return nil, err
		}
		var response struct {
			Data   []cleanupNative `json:"data"`
			Cursor struct {
				Next string `json:"next"`
			} `json:"cursor"`
		}
		if err := json.Unmarshal(raw, &response); err != nil {
			return nil, err
		}
		if response.Data == nil || len(response.Data) > 500 {
			return nil, errors.New("invalid cleanup session listing")
		}
		for _, row := range response.Data {
			if row.Parent == "" {
				row.Parent = row.ParentAlias
			}
			if row.ID == "" {
				return nil, errors.New("cleanup session identity missing")
			}
			if _, ok := inventory.Rows[row.ID]; ok {
				return nil, errors.New("duplicate cleanup session identity")
			}
			inventory.Rows[row.ID] = row
		}
		if response.Cursor.Next == "" {
			break
		}
		if len(response.Data) == 0 || seen[response.Cursor.Next] || page == 49 {
			return nil, errors.New("incomplete cleanup session listing")
		}
		cursor = response.Cursor.Next
		seen[cursor] = true
	}
	active, err := cleanupActive(ctx, api)
	if err != nil {
		return nil, err
	}
	inventory.Active = active
	return inventory, nil
}

func cleanupActive(ctx context.Context, api API) (map[string]json.RawMessage, error) {
	raw, err := api.RoundTrip(ctx, "GET", "/api/session/active", nil)
	if err != nil {
		return nil, err
	}
	var response struct {
		Data map[string]json.RawMessage `json:"data"`
	}
	if err := json.Unmarshal(raw, &response); err != nil {
		return nil, err
	}
	if response.Data == nil {
		return nil, errors.New("invalid active-session listing")
	}
	return response.Data, nil
}

func (c *CleanupCatalog) Tree(root string) map[string]bool {
	tree := map[string]bool{root: true}
	children := map[string][]string{}
	for _, row := range c.Rows {
		children[row.Parent] = append(children[row.Parent], row.ID)
	}
	queue := []string{root}
	for len(queue) > 0 {
		id := queue[0]
		queue = queue[1:]
		for _, child := range children[id] {
			if !tree[child] {
				tree[child] = true
				queue = append(queue, child)
			}
		}
	}
	return tree
}

func oldNative(row cleanupNative, before uint64) bool {
	return row.Time.Updated > 0 && max(row.Time.Created, row.Time.Updated)/1000 < before
}

func CleanupLocalEligible(session backendstore.Object, before uint64) bool {
	if session.String("provider") != "opencode" || session.Bool("archived") || session.String("native_session_id") == "" {
		return false
	}
	if status := session.String("status"); status != "idle" && status != "failed" {
		return false
	}
	updated, err := session.Int("updated_at")
	if err != nil || updated <= 0 {
		return false
	}
	created, err := session.Int("created_at")
	if err != nil {
		return false
	}
	reply, err := session.Int("last_reply_at")
	if err != nil {
		return false
	}
	if uint64(max(updated, created, reply)) >= before {
		return false
	}
	var queued []json.RawMessage
	if len(session["queued_messages"]) > 0 {
		if json.Unmarshal(session["queued_messages"], &queued) != nil || len(queued) > 0 {
			return false
		}
	}
	var turns []backendstore.Object
	if len(session["turns"]) > 0 && json.Unmarshal(session["turns"], &turns) != nil {
		return false
	}
	for _, turn := range turns {
		if turn.String("status") == "running" {
			return false
		}
		if raw := turn["submission"]; len(raw) > 0 && string(raw) != "null" {
			receipt, err := backendstore.Decode(raw)
			if err != nil {
				return false
			}
			switch receipt.String("state") {
			case "preparing", "dispatching", "unknown":
				return false
			}
		}
	}
	return true
}

func (c *CleanupCatalog) Eligible(session backendstore.Object, locals []backendstore.Object, blocked map[string]bool, before uint64) map[string]bool {
	if blocked[session.String("id")] || !CleanupLocalEligible(session, before) {
		return nil
	}
	tree := c.Tree(session.String("native_session_id"))
	for id := range tree {
		if _, ok := c.Active[id]; ok {
			return nil
		}
		if row, ok := c.Rows[id]; ok && !oldNative(row, before) {
			return nil
		}
	}
	for _, row := range locals {
		if tree[row.String("native_session_id")] && (blocked[row.String("id")] || !CleanupLocalEligible(row, before)) {
			return nil
		}
	}
	return tree
}

// RecheckCleanup makes no local changes. A false result is a safe skip, not
// permission to delete from a stale inventory.
func RecheckCleanup(ctx context.Context, api API, tree map[string]bool, before uint64) (bool, error) {
	for id := range tree {
		raw, err := api.RoundTrip(ctx, "GET", "/api/session/"+url.PathEscape(id), nil)
		if err != nil {
			var failure *backendengine.HTTPError
			if errors.As(err, &failure) && failure.Status == 404 {
				continue
			}
			return false, err
		}
		body, err := data(raw)
		if err != nil {
			return false, err
		}
		var row cleanupNative
		if err := json.Unmarshal(body, &row); err != nil {
			return false, err
		}
		if row.ID != id {
			return false, errors.New("cleanup recheck returned a different session")
		}
		if !oldNative(row, before) {
			return false, nil
		}
	}
	active, err := cleanupActive(ctx, api)
	if err != nil {
		return false, err
	}
	for id := range tree {
		if _, ok := active[id]; ok {
			return false, nil
		}
	}
	return true, nil
}

func DeleteCleanupRoot(ctx context.Context, api API, id string) error {
	_, err := api.RoundTrip(ctx, "DELETE", "/api/session/"+url.PathEscape(id), nil)
	if err != nil {
		var failure *backendengine.HTTPError
		if errors.As(err, &failure) && failure.Status == 404 {
			return nil
		}
	}
	return err
}
