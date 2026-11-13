package backendengine

import (
	"encoding/json"
	"strings"
)

func ErrorText(raw json.RawMessage) string {
	var find func(json.RawMessage, int) string
	find = func(value json.RawMessage, depth int) string {
		if depth > 4 || len(value) == 0 || string(value) == "null" {
			return ""
		}
		var text string
		if json.Unmarshal(value, &text) == nil {
			return strings.TrimSpace(text)
		}
		var fields map[string]json.RawMessage
		if json.Unmarshal(value, &fields) != nil {
			return ""
		}
		for _, key := range []string{"message", "error", "data", "detail"} {
			if text := find(fields[key], depth+1); text != "" {
				return text
			}
		}
		return ""
	}
	if text := find(raw, 0); text != "" {
		return text
	}
	if len(raw) > 0 && string(raw) != "null" {
		return string(raw)
	}
	return ""
}
