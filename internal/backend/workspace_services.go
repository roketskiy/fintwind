package backend

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"reflect"
	"strings"
	"time"

	"fintwind-go/internal/backendservices"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/backendworkspace"
)

func registryData(raw json.RawMessage) (json.RawMessage, error) {
	if len(raw) == 0 {
		return nil, errors.New("empty registry response")
	}
	if raw[0] == '[' {
		return raw, nil
	}
	row, err := backendstore.Decode(raw)
	if err != nil {
		return nil, err
	}
	if len(row["data"]) == 0 {
		return nil, errors.New("invalid registry response")
	}
	return row["data"], nil
}

func readRegisteredCommands(ctx context.Context, api backendservices.API, directory string) ([]backendworkspace.SlashCommand, []backendworkspace.SlashCommand, bool) {
	work, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	query := "?location%5Bdirectory%5D=" + url.QueryEscape(directory)
	call := func(path string) (json.RawMessage, error) {
		child, done := context.WithTimeout(work, 5*time.Second)
		defer done()
		return api.RoundTrip(child, "GET", path+query, nil)
	}
	_, _ = call("/api/config")
	var previous []backendworkspace.SlashCommand
	var commands []backendworkspace.SlashCommand
	stable := 0
	delay := 500 * time.Millisecond
	for work.Err() == nil {
		raw, err := call("/api/command")
		var next []backendworkspace.SlashCommand
		if err == nil {
			body, e := registryData(raw)
			err = e
			if err == nil {
				var rows []backendstore.Object
				err = json.Unmarshal(body, &rows)
				if err == nil {
					next = []backendworkspace.SlashCommand{}
					for _, row := range rows {
						name := row.String("name")
						if name != "" && len(name) <= 256 && len(next) < 500 {
							description := row.String("description")
							if len(description) > 4096 {
								description = description[:4096]
							}
							next = append(next, backendworkspace.SlashCommand{Name: name, Description: description})
						}
					}
				}
			}
		}
		if err == nil {
			if previous != nil && reflect.DeepEqual(previous, next) {
				stable++
			} else {
				stable = 0
			}
			previous = next
			commands = next
			if stable >= 2 {
				break
			}
		} else {
			previous = nil
			stable = 0
		}
		timer := time.NewTimer(delay)
		select {
		case <-work.Done():
			timer.Stop()
		case <-timer.C:
		}
		delay = min(2*time.Second, delay*2)
	}
	// Skill listing is a separate authoritative operation. Its successful empty
	// array intentionally removes filesystem-only skill guesses.
	skillsCtx, done := context.WithTimeout(ctx, 15*time.Second)
	defer done()
	_, err := api.RoundTrip(skillsCtx, "GET", "/api/config"+query, nil)
	if err != nil {
		return commands, nil, false
	}
	raw, err := api.RoundTrip(skillsCtx, "GET", "/api/skill"+query, nil)
	if err != nil {
		return commands, nil, false
	}
	body, err := registryData(raw)
	if err != nil {
		return commands, nil, false
	}
	var rows []backendstore.Object
	if json.Unmarshal(body, &rows) != nil || string(body) == "null" || len(rows) > 500 {
		return commands, nil, false
	}
	skills := []backendworkspace.SlashCommand{}
	for _, row := range rows {
		id := row.String("id")
		description := row.String("description")
		if id == "" || len(id) > 256 || len(description) > 4096 {
			return commands, nil, false
		}
		skills = append(skills, backendworkspace.SlashCommand{Name: id, Description: description})
	}
	return commands, skills, true
}

func (s *Server) workspaceService(ctx context.Context, raw json.RawMessage) (backendstore.Object, bool, error) {
	raw, err := backendworkspace.NormalizeOperation(raw)
	if err != nil {
		return nil, true, err
	}
	op, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	switch op.String("type") {
	case "inspectPullRequests":
		return s.inspectPullRequests(ctx, op)
	case "discoverSlashCommands":
		binary := strings.TrimSpace(op.String("binary"))
		if binary == "" {
			return nil, false, nil
		}
		delete(op, "binary")
		localRaw, _ := json.Marshal(op)
		root, localRaw, err := s.workspaceOperation(localRaw)
		if err != nil {
			return nil, true, err
		}
		result, err := backendworkspace.DispatchWithQueries(ctx, root, localRaw, &s.queries)
		if err != nil {
			return nil, true, err
		}
		var local struct {
			Commands []backendworkspace.SlashCommand `json:"commands"`
		}
		if err := json.Unmarshal(result, &local); err != nil {
			return nil, true, err
		}
		directory := op.String("projectRoot")
		api, err := s.servicePool.acquire(ctx, backendstore.Fields(map[string]any{"binary": binary, "directory": directory}))
		if err != nil {
			if ctx.Err() != nil {
				return nil, true, ctx.Err()
			}
			return backendstore.Reply("workspace", backendstore.Fields(map[string]any{"result": json.RawMessage(result)})), true, nil
		}
		commands, skills, ready := readRegisteredCommands(ctx, api, directory)
		if err := ctx.Err(); err != nil {
			return nil, true, err
		}
		return backendstore.Reply("workspace", backendstore.Fields(map[string]any{"result": map[string]any{"type": "slashCommands", "commands": backendworkspace.MergeRegisteredCommands(local.Commands, commands, skills, ready)}})), true, nil
	case "generateCommitMessage":
		return s.generateCommitMessage(ctx, op)
	}
	return nil, false, nil
}
