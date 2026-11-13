package backendstore

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
)

func mergeSnapshot(ctx context.Context, tx *sql.Tx, incoming Object) (Object, bool, error) {
	if err := fenceHistory(ctx, tx, incoming); err != nil {
		return nil, false, err
	}
	var raw string
	err := tx.QueryRowContext(ctx, `SELECT data FROM session_details WHERE session_id=?`, incoming.String("id")).Scan(&raw)
	if errors.Is(err, sql.ErrNoRows) {
		return incoming, false, nil
	}
	if err != nil {
		return nil, false, err
	}
	existing, err := Decode([]byte(raw))
	if err != nil {
		return nil, false, err
	}
	var oldTurns, newTurns []Object
	if err := json.Unmarshal(existing["turns"], &oldTurns); err != nil && len(existing["turns"]) > 0 {
		return nil, false, err
	}
	if err := json.Unmarshal(incoming["turns"], &newTurns); err != nil && len(incoming["turns"]) > 0 {
		return nil, false, err
	}
	byID := map[string]Object{}
	for _, turn := range newTurns {
		byID[turn.String("id")] = turn
	}
	stale, preparing := false, false
	for _, turn := range oldTurns {
		receipt := Object{}
		if len(turn["submission"]) > 0 && string(turn["submission"]) != "null" {
			if err := json.Unmarshal(turn["submission"], &receipt); err != nil {
				return nil, false, err
			}
		}
		state := receipt.String("state")
		if state == "preparing" {
			preparing = true
		}
		candidate := byID[turn.String("id")]
		if receipt != nil && len(receipt) > 0 {
			if (state == "dispatching" || state == "unknown" || turn.String("status") == "running") && candidate == nil {
				stale = true
			}
			if turn.String("status") != "running" && candidate.String("status") == "running" {
				stale = true
			}
			if candidate != nil {
				candidate["submission"] = turn["submission"]
			}
		}
		if candidate != nil && len(turn["checkpoint"]) > 0 {
			candidate["checkpoint"] = turn["checkpoint"]
		}
	}
	var before, after struct {
		Runtime  string `json:"runtime_id"`
		Epoch    string `json:"epoch"`
		Sequence uint64 `json:"sequence"`
	}
	_ = json.Unmarshal(existing["runtime_event_cursor"], &before)
	_ = json.Unmarshal(incoming["runtime_event_cursor"], &after)
	if before.Sequence > 0 && (after.Runtime != before.Runtime || after.Epoch != before.Epoch || after.Sequence < before.Sequence) {
		stale = true
	}
	if preparing || stale {
		if !preparing {
			oldTime, _ := existing.Int("updated_at")
			newTime, _ := incoming.Int("updated_at")
			if newTime >= oldTime {
				for _, key := range []string{"title", "auto_title", "archived"} {
					if incoming[key] != nil {
						existing[key] = incoming[key]
					}
				}
			}
		}
		var queued, incomingQueue []Object
		_ = json.Unmarshal(existing["queued_messages"], &queued)
		_ = json.Unmarshal(incoming["queued_messages"], &incomingQueue)
		seen := map[string]bool{}
		for _, item := range queued {
			seen[item.String("id")] = true
		}
		for _, item := range incomingQueue {
			if !seen[item.String("id")] {
				queued = append(queued, item)
				seen[item.String("id")] = true
			}
		}
		if queued == nil {
			queued = []Object{}
		}
		existing.Set("queued_messages", queued)
		encoded, err := json.Marshal(existing)
		if err != nil {
			return nil, false, err
		}
		if _, err := tx.ExecContext(ctx, `UPDATE session_details SET data=? WHERE session_id=?`, string(encoded), incoming.String("id")); err != nil {
			return nil, false, err
		}
		if !preparing {
			if _, err := tx.ExecContext(ctx, `UPDATE sessions SET title=?,auto_title=?,archived=? WHERE id=?`, existing.String("title"), existing.Nullable("auto_title"), existing.Bool("archived"), incoming.String("id")); err != nil {
				return nil, false, err
			}
		}
		return nil, true, nil
	}
	incoming.Set("turns", newTurns)
	if saved := existing["background_work"]; len(saved) > 0 {
		incoming["background_work"] = saved
	}
	if saved := existing["_fintwind_shell_cursors"]; len(saved) > 0 {
		incoming["_fintwind_shell_cursors"] = saved
	}
	return incoming, false, nil
}
