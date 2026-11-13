package backend

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"

	"fintwind-go/internal/backendstore"
)

func readObject(path string) (backendstore.Object, error) {
	raw, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return backendstore.Object{}, nil
	}
	if err != nil {
		return nil, err
	}
	return backendstore.Decode(raw)
}

func writeObject(path string, object backendstore.Object) error {
	raw, err := json.Marshal(object)
	if err != nil {
		return err
	}
	file, err := os.CreateTemp(filepath.Dir(path), ".backend-*.tmp")
	if err != nil {
		return err
	}
	name := file.Name()
	defer os.Remove(name)
	if err := file.Chmod(0o600); err != nil {
		file.Close()
		return err
	}
	if _, err := file.Write(raw); err != nil {
		file.Close()
		return err
	}
	if err := file.Sync(); err != nil {
		file.Close()
		return err
	}
	if err := file.Close(); err != nil {
		return err
	}
	return os.Rename(name, path)
}

func (s *Server) files(command backendstore.Object) (backendstore.Object, error) {
	s.fileMu.Lock()
	defer s.fileMu.Unlock()
	kind := command.String("type")
	path := s.settingsPath
	response, field := "settings", "settings"
	if kind == "loadComposerDrafts" || kind == "saveComposerDrafts" || kind == "applyComposerDraftChanges" {
		path = filepath.Join(s.Directory, "composer-drafts.json")
		response, field = "composerDrafts", "drafts"
	}
	object, err := readObject(path)
	if err != nil {
		return nil, err
	}
	switch kind {
	case "updateSettings", "saveComposerDrafts":
		var generation int64
		if kind == "saveComposerDrafts" {
			generation, err = command.Int("generation")
			if err != nil {
				return nil, err
			}
			if generation < s.draftGeneration {
				return backendstore.Reply("ack", nil), nil
			}
		}
		object, err = backendstore.Decode(command[field])
		if err != nil {
			return nil, err
		}
		if err := writeObject(path, object); err != nil {
			return nil, err
		}
		if kind == "saveComposerDrafts" {
			s.draftGeneration = generation
			return backendstore.Reply("ack", nil), nil
		}
	case "applyComposerDraftChanges":
		var changes []struct {
			Target backendstore.Object `json:"target"`
			Draft  json.RawMessage     `json:"draft"`
		}
		if err := json.Unmarshal(command["changes"], &changes); err != nil {
			return nil, errors.New("invalid draft changes")
		}
		for _, change := range changes {
			var group, key string
			switch change.Target.String("type") {
			case "newSession":
				group = "new_sessions"
				key = "projectId"
			case "session":
				group = "sessions"
				key = "sessionId"
			default:
				return nil, errors.New("invalid draft target")
			}
			id, err := change.Target.ID(key)
			if err != nil {
				return nil, err
			}
			drafts := backendstore.Object{}
			if object[group] != nil {
				if err := json.Unmarshal(object[group], &drafts); err != nil {
					return nil, errors.New("invalid stored drafts")
				}
			}
			if len(change.Draft) == 0 || string(change.Draft) == "null" {
				delete(drafts, id)
			} else {
				draft, err := backendstore.Decode(change.Draft)
				if err != nil {
					return nil, err
				}
				var attachments, references []json.RawMessage
				if len(draft["attachments"]) > 0 {
					if err := json.Unmarshal(draft["attachments"], &attachments); err != nil {
						return nil, errors.New("invalid draft attachments")
					}
				}
				if len(draft["references"]) > 0 {
					if err := json.Unmarshal(draft["references"], &references); err != nil {
						return nil, errors.New("invalid draft references")
					}
				}
				if draft.String("text") == "" && len(attachments) == 0 && len(references) == 0 {
					delete(drafts, id)
				} else {
					drafts[id] = change.Draft
				}
			}
			object.Set(group, drafts)
		}
		if err := writeObject(path, object); err != nil {
			return nil, err
		}
		s.draftGeneration++
		return backendstore.Reply("ack", nil), nil
	case "updateSetting":
		key := command.String("key")
		raw := command["value"]
		switch key {
		case "network_proxy_url", "opencode_binary_path", "general_workspace_path":
			var value *string
			if err := json.Unmarshal(raw, &value); err != nil {
				return nil, errors.New("invalid setting value")
			}
		case "scheduled_tools_enabled", "auto_compact_enabled":
			var value bool
			if string(raw) == "null" || json.Unmarshal(raw, &value) != nil {
				return nil, errors.New("invalid setting value")
			}
		case "auto_compact_threshold_tokens":
			value, err := command.Int("value")
			if err != nil || value < 10000 || value > 100000000 {
				return nil, errors.New("invalid setting value")
			}
		default:
			return nil, errors.New("unsupported setting key")
		}
		if string(raw) == "null" {
			delete(object, key)
		} else {
			object[key] = raw
		}
		if err := writeObject(path, object); err != nil {
			return nil, err
		}
	}
	return backendstore.Reply(response, backendstore.Fields(map[string]any{field: object})), nil
}
