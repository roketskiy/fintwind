package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

func bytesReader(b []byte) *bytes.Reader { return bytes.NewReader(b) }

func stripJSONComments(input string) string {
	var out strings.Builder
	inString := false
	escaped := false
	lineComment := false
	blockComment := false
	for i := 0; i < len(input); i++ {
		c := input[i]
		if lineComment {
			if c == '\n' {
				lineComment = false
				out.WriteByte(c)
			}
			continue
		}
		if blockComment {
			if c == '*' && i+1 < len(input) && input[i+1] == '/' {
				blockComment = false
				i++
			}
			continue
		}
		if inString {
			out.WriteByte(c)
			if escaped {
				escaped = false
			} else if c == '\\' {
				escaped = true
			} else if c == '"' {
				inString = false
			}
			continue
		}
		if c == '"' {
			inString = true
			out.WriteByte(c)
			continue
		}
		if c == '/' && i+1 < len(input) {
			next := input[i+1]
			if next == '/' {
				lineComment = true
				i++
				continue
			}
			if next == '*' {
				blockComment = true
				i++
				continue
			}
		}
		out.WriteByte(c)
	}
	return out.String()
}

func providerConfigPath() string {
	if dir := os.Getenv("OPENCODE_CONFIG_DIR"); dir != "" {
		jsonc := filepath.Join(dir, "opencode.jsonc")
		if st, err := os.Stat(jsonc); err == nil && !st.IsDir() {
			return jsonc
		}
		return filepath.Join(dir, "opencode.json")
	}
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		home = os.TempDir()
	}
	jsonPath := filepath.Join(home, ".config", "opencode", "opencode.json")
	jsonc := filepath.Join(home, ".config", "opencode", "opencode.jsonc")
	if st, err := os.Stat(jsonc); err == nil && !st.IsDir() {
		return jsonc
	}
	return jsonPath
}

func readOpencodeDocument(path string) (map[string]any, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var document map[string]any
	decoder := json.NewDecoder(bytesReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&document); err != nil {
		// Try stripping // and /* */ comments for jsonc.
		stripped := stripJSONComments(string(raw))
		decoder = json.NewDecoder(bytesReader([]byte(stripped)))
		decoder.UseNumber()
		if err := decoder.Decode(&document); err != nil {
			return nil, err
		}
	}
	if document == nil {
		document = map[string]any{}
	}
	return document, nil
}

func settingsProviderFromEntry(key string, entry map[string]any, disabled map[string]bool, native bool) settingsProvider {
	provider := settingsProvider{ID: key, Entry: cloneJSONMap(entry), Enabled: !disabled[key], Native: native}
	return provider
}

func loadOpencodeProviders() ([]settingsProvider, error) {
	path := providerConfigPath()
	document, err := readOpencodeDocument(path)
	if err != nil {
		if os.IsNotExist(err) {
			return []settingsProvider{}, nil
		}
		return nil, err
	}
	disabled := map[string]bool{}
	if list, ok := document["disabled_providers"].([]any); ok {
		for _, item := range list {
			if name, ok := item.(string); ok {
				disabled[name] = true
			}
		}
	}
	var out []settingsProvider
	if providers, ok := document["provider"].(map[string]any); ok {
		keys := make([]string, 0, len(providers))
		for key := range providers {
			keys = append(keys, key)
		}
		sort.Strings(keys)
		for _, key := range keys {
			entry, _ := providers[key].(map[string]any)
			if entry == nil {
				entry = map[string]any{}
			}
			out = append(out, settingsProviderFromEntry(key, entry, disabled, false))
		}
	}
	if native, ok := document["providers"].(map[string]any); ok {
		keys := make([]string, 0, len(native))
		for key := range native {
			keys = append(keys, key)
		}
		sort.Strings(keys)
		for _, key := range keys {
			entry, ok := native[key].(map[string]any)
			if !ok {
				continue
			}
			models, _ := entry["models"].(map[string]any)
			if models != nil {
				valid := true
				for _, spec := range models {
					if _, ok := spec.(map[string]any); !ok {
						valid = false
						break
					}
				}
				if !valid {
					continue
				}
			}
			// Skip if a legacy entry with the same key already covers it.
			skip := false
			for _, existing := range out {
				if existing.ID == key {
					skip = true
					break
				}
			}
			if skip {
				continue
			}
			out = append(out, settingsProviderFromEntry(key, entry, disabled, true))
		}
	}
	if out == nil {
		out = []settingsProvider{}
	}
	return out, nil
}

func providerEntryForSave(provider settingsProvider, catalog *modelsDevTable) map[string]any {
	entry := cloneJSONMap(provider.Entry)
	if entry == nil {
		entry = map[string]any{}
	}
	entry["name"] = settingsProviderName(provider)
	_ = catalog
	return entry
}

func saveOpencodeProviders(providers []settingsProvider, catalog *modelsDevTable) error {
	path := providerConfigPath()
	document, err := readOpencodeDocument(path)
	if err != nil {
		if !os.IsNotExist(err) {
			return err
		}
		document = map[string]any{"$schema": "https://opencode.ai/config.json"}
	}
	managed := map[string]bool{}
	for _, provider := range providers {
		managed[provider.ID] = true
	}
	providerMap := map[string]any{}
	nativeMap, _ := document["providers"].(map[string]any)
	if nativeMap == nil {
		nativeMap = map[string]any{}
	} else {
		nativeMap = cloneJSONMap(nativeMap)
	}
	for _, provider := range providers {
		delete(nativeMap, provider.ID)
	}
	if existing, ok := document["providers"].(map[string]any); ok {
		for id, entry := range existing {
			if entryMap, ok := entry.(map[string]any); ok {
				if _, hasModels := entryMap["models"]; hasModels && !managed[id] {
					delete(nativeMap, id)
				}
			}
		}
	}
	for _, provider := range providers {
		entry := providerEntryForSave(provider, catalog)
		if provider.Native {
			nativeMap[provider.ID] = entry
		} else {
			providerMap[provider.ID] = entry
		}
	}
	document["provider"] = providerMap
	if len(nativeMap) > 0 {
		document["providers"] = nativeMap
	} else {
		delete(document, "providers")
	}
	var disabled []string
	if list, ok := document["disabled_providers"].([]any); ok {
		for _, item := range list {
			if name, ok := item.(string); ok && !managed[name] {
				disabled = append(disabled, name)
			}
		}
	}
	for _, provider := range providers {
		if !provider.Enabled {
			disabled = append(disabled, provider.ID)
		}
	}
	sort.Strings(disabled)
	if len(disabled) == 0 {
		delete(document, "disabled_providers")
	} else {
		disabledAny := make([]any, 0, len(disabled))
		for _, name := range disabled {
			disabledAny = append(disabledAny, name)
		}
		document["disabled_providers"] = disabledAny
	}
	raw, err := json.MarshalIndent(document, "", "  ")
	if err != nil {
		return err
	}
	if dir := filepath.Dir(path); dir != "" {
		_ = os.MkdirAll(dir, 0o755)
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, append(raw, '\n'), 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

func isKeyReference(key string) bool {
	key = strings.TrimSpace(key)
	return strings.HasPrefix(key, "{env:") || strings.HasPrefix(key, "{file:")
}

func cloneJSONMap(value map[string]any) map[string]any {
	if value == nil {
		return nil
	}
	raw, err := json.Marshal(value)
	if err != nil {
		return map[string]any{}
	}
	var out map[string]any
	decoder := json.NewDecoder(bytesReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&out); err != nil {
		return map[string]any{}
	}
	if out == nil {
		return map[string]any{}
	}
	return out
}

func jsonObject(parent map[string]any, key string) map[string]any {
	if object, ok := parent[key].(map[string]any); ok {
		return object
	}
	object := map[string]any{}
	parent[key] = object
	return object
}
