package backendservices

import (
	"encoding/json"
	"reflect"
	"strings"

	"fintwind-go/internal/backendstore"
)

func modelVariants(entry backendstore.Object) []backendstore.Object {
	var variants []backendstore.Object
	_ = json.Unmarshal(entry["variants"], &variants)
	options := make([]backendstore.Object, 0, len(variants))
	for _, variant := range variants {
		id := strings.TrimSpace(variant.String("id"))
		if id != "" {
			options = append(options, backendstore.Fields(map[string]any{"id": id, "label": id}))
		}
	}
	return options
}

func modelDefaultVariant(entry backendstore.Object) *string {
	var variants []backendstore.Object
	var settings map[string]any
	_ = json.Unmarshal(entry["variants"], &variants)
	_ = json.Unmarshal(entry["settings"], &settings)
	for _, variant := range variants {
		var values map[string]any
		if json.Unmarshal(variant["settings"], &values) != nil || len(values) == 0 {
			continue
		}
		matched := true
		for key, value := range values {
			if existing, ok := settings[key]; !ok || !reflect.DeepEqual(existing, value) {
				matched = false
				break
			}
		}
		if id := strings.TrimSpace(variant.String("id")); matched && id != "" {
			return &id
		}
	}
	return nil
}
