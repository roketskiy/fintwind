package backendworkspace

import (
	"encoding/json"
	"errors"
	"strings"
)

// WorkspaceOperation accepts snake_case fields, unlike the enclosing
// Command. Older Go callers already send camelCase; accept either spelling
// without accepting contradictory duplicate aliases.
func normalizeOperation(raw json.RawMessage) (json.RawMessage, error) {
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(raw, &fields); err != nil {
		return nil, err
	}
	for key, value := range fields {
		if !strings.Contains(key, "_") {
			continue
		}
		parts := strings.Split(key, "_")
		camel := parts[0]
		for _, part := range parts[1:] {
			if part != "" {
				camel += strings.ToUpper(part[:1]) + part[1:]
			}
		}
		if existing, ok := fields[camel]; ok && string(existing) != string(value) {
			return nil, errors.New("conflicting workspace field aliases")
		}
		fields[camel] = value
		delete(fields, key)
	}
	return json.Marshal(fields)
}

func NormalizeOperation(raw json.RawMessage) (json.RawMessage, error) { return normalizeOperation(raw) }
