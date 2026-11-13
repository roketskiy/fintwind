package backendusage

import (
	"context"
	"encoding/json"
	"errors"
	"math"
	"net/url"
	"sort"
	"time"

	"fintwind-go/internal/backendstore"
)

type rowFacts struct {
	Kind          string    `json:"kind"`
	Parent        *string   `json:"parent"`
	Created       uint64    `json:"created"`
	Updated       uint64    `json:"updated"`
	UpdatedMillis uint64    `json:"updated_millis"`
	Model         *string   `json:"model"`
	Directory     *string   `json:"directory"`
	Cost          *float64  `json:"cost"`
	Tokens        [5]uint64 `json:"tokens"`
}
type sessionFact struct {
	ID    string
	Facts rowFacts
	Split *sessionSplit
}

func parseFacts(row backendstore.Object) (sessionFact, error) {
	id := row.String("id")
	timing := object(row["time"])
	created, hasCreated := optionalUint(timing["created"])
	updated, hasUpdated := optionalUint(timing["updated"])
	if !hasCreated {
		created = updated
	}
	if !hasUpdated {
		updated = created / 1000 * 1000
	}
	if id == "" || (!hasCreated && !hasUpdated) || max(created, updated)/1000 > math.MaxInt64 {
		return sessionFact{}, errors.New("invalid usage session facts")
	}
	facts := rowFacts{Kind: "TopLevel", Created: created / 1000, Updated: updated / 1000, UpdatedMillis: updated, Model: reportedModel(row["model"])}
	if model := object(row["model"]); model.String("providerID") != "" && model.String("id") != "" {
		name := model.String("providerID") + "/" + model.String("id")
		facts.Model = &name
	}
	fork := row.String("forkSessionID")
	if fork == "" {
		fork = row.String("fork_session_id")
	}
	parent := row.String("parentID")
	if parent == "" {
		parent = row.String("parent_id")
	}
	if fork != "" {
		facts.Kind = "Fork"
	} else if parent != "" {
		facts.Kind = "SubAgent"
		facts.Parent = &parent
	}
	if location := object(row["location"]); location.String("directory") != "" {
		directory := location.String("directory")
		facts.Directory = &directory
	}
	if raw := row["cost"]; len(raw) > 0 && string(raw) != "null" {
		var cost float64
		if json.Unmarshal(raw, &cost) == nil && !math.IsNaN(cost) && !math.IsInf(cost, 0) {
			facts.Cost = &cost
		}
	}
	tokens := object(row["tokens"])
	cache := object(tokens["cache"])
	facts.Tokens = [5]uint64{uintValue(tokens["input"]), uintValue(tokens["output"]), uintValue(tokens["reasoning"]), uintValue(cache["read"]), uintValue(cache["write"])}
	return sessionFact{ID: id, Facts: facts}, nil
}

func readListing(ctx context.Context, api API) ([]sessionFact, int, error) {
	cursor := ""
	seenCursor := map[string]bool{}
	seenID := map[string]bool{}
	sessions := []sessionFact{}
	bytes := 0
	scanned := 0
	for page := 0; page < 50; page++ {
		path := "/api/session?limit=500"
		if cursor != "" {
			path += "&cursor=" + url.QueryEscape(cursor)
		}
		get, stop := context.WithTimeout(ctx, 15*time.Second)
		raw, err := api.RoundTrip(get, "GET", path, nil)
		stop()
		if err != nil {
			return nil, scanned, err
		}
		bytes += len(raw)
		if bytes > 64<<20 {
			return nil, scanned, errors.New("usage listing byte budget exceeded")
		}
		var response struct {
			Data   []backendstore.Object `json:"data"`
			Cursor struct {
				Next string `json:"next"`
			} `json:"cursor"`
		}
		if json.Unmarshal(raw, &response) != nil || response.Data == nil || len(response.Data) > 500 {
			return nil, scanned, errors.New("invalid usage session listing")
		}
		scanned += len(response.Data)
		for _, row := range response.Data {
			session, err := parseFacts(row)
			if err != nil {
				return nil, scanned, err
			}
			if seenID[session.ID] {
				return nil, scanned, errors.New("duplicate usage session identity")
			}
			seenID[session.ID] = true
			sessions = append(sessions, session)
		}
		next := response.Cursor.Next
		if next == "" {
			return sessions, scanned, nil
		}
		if seenCursor[next] || len(response.Data) == 0 || page == 49 {
			return nil, scanned, errors.New("usage session listing incomplete")
		}
		seenCursor[next] = true
		cursor = next
	}
	return nil, scanned, errors.New("usage session listing exceeded page budget")
}

// orderedFacts visits top-level sessions and forks first, then child sessions
// by ancestry depth, so nested sub-agent use folds once into its root entry.
func orderedFacts(rows []sessionFact) []sessionFact {
	byID := map[string]rowFacts{}
	for _, row := range rows {
		byID[row.ID] = row.Facts
	}
	depth := func(row sessionFact) int {
		if row.Facts.Kind != "SubAgent" {
			return -1
		}
		n := 0
		parent := row.Facts.Parent
		for parent != nil && n <= len(rows) {
			n++
			facts, ok := byID[*parent]
			if !ok {
				break
			}
			parent = facts.Parent
		}
		return n
	}
	depths := map[string]int{}
	for _, row := range rows {
		depths[row.ID] = depth(row)
	}
	sort.SliceStable(rows, func(i, j int) bool {
		a, b := depths[rows[i].ID], depths[rows[j].ID]
		if a != b {
			return a < b
		}
		return rows[i].ID < rows[j].ID
	})
	return rows
}
