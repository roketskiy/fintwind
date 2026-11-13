package main

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

const modelsDevReuseWindow = 10 * time.Minute

type modelsDevModel struct {
	ID               string   `json:"id"`
	Provider         string   `json:"provider"`
	Name             *string  `json:"name,omitempty"`
	ContextWindow    *uint64  `json:"context_window,omitempty"`
	OutputLimit      *uint64  `json:"output_limit,omitempty"`
	InputModalities  []string `json:"input_modalities,omitempty"`
	OutputModalities []string `json:"output_modalities,omitempty"`
}

type modelsDevProvider struct {
	ID     string           `json:"id"`
	Name   string           `json:"name"`
	API    *string          `json:"api,omitempty"`
	NPM    *string          `json:"npm,omitempty"`
	Models []modelsDevModel `json:"models"`
}

type modelsDevProviders struct {
	FetchedAt int64                        `json:"fetched_at"`
	Providers map[string]modelsDevProvider `json:"providers"`
	SortedIDs []string                     `json:"sorted_ids"`
}

func (p *modelsDevProviders) age() time.Duration {
	if p == nil {
		return time.Hour * 24 * 365
	}
	now := time.Now().UnixMilli()
	delta := now - p.FetchedAt
	if delta < 0 {
		delta = 0
	}
	return time.Duration(delta) * time.Millisecond
}

func (p *modelsDevProviders) get(id string) *modelsDevProvider {
	if p == nil {
		return nil
	}
	provider, ok := p.Providers[id]
	if !ok {
		return nil
	}
	return &provider
}

func (p *modelsDevProviders) sorted() []modelsDevProvider {
	if p == nil {
		return nil
	}
	out := make([]modelsDevProvider, 0, len(p.SortedIDs))
	for _, id := range p.SortedIDs {
		if provider, ok := p.Providers[id]; ok {
			out = append(out, provider)
		}
	}
	return out
}

type modelsDevTable struct {
	FetchedAt int64                       `json:"fetched_at"`
	Models    map[string][]modelsDevModel `json:"models"`
}

func (t *modelsDevTable) age() time.Duration {
	if t == nil {
		return time.Hour * 24 * 365
	}
	now := time.Now().UnixMilli()
	delta := now - t.FetchedAt
	if delta < 0 {
		delta = 0
	}
	return time.Duration(delta) * time.Millisecond
}

type recordBasics struct {
	name             *string
	contextWindow    *uint64
	outputLimit      *uint64
	inputModalities  []string
	outputModalities []string
}

func normalizeModelID(id string) string {
	var b strings.Builder
	for _, r := range id {
		if (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (r >= '0' && r <= '9') {
			if r >= 'A' && r <= 'Z' {
				r += 'a' - 'A'
			}
			b.WriteRune(r)
		}
	}
	return b.String()
}

func sameModelID(left, right string) bool {
	return left == right || normalizeModelID(left) == normalizeModelID(right)
}

func modelsDevCacheDir() string {
	if dir := os.Getenv("FINTWIND_MODEL_CACHE_DIR"); dir != "" {
		return dir
	}
	if cache, err := os.UserCacheDir(); err == nil && cache != "" {
		return filepath.Join(cache, "fintwind", "models")
	}
	return filepath.Join(os.TempDir(), "fintwind-models")
}

func modelsDevTableCachePath() string {
	return filepath.Join(modelsDevCacheDir(), "models-dev.json")
}

func modelsDevProvidersCachePath() string {
	return filepath.Join(modelsDevCacheDir(), "models-dev-providers.json")
}

func cachedModelsDevTable() *modelsDevTable {
	raw, err := os.ReadFile(modelsDevTableCachePath())
	if err != nil {
		return nil
	}
	var table modelsDevTable
	if json.Unmarshal(raw, &table) != nil || table.Models == nil {
		return nil
	}
	return &table
}

func writeCachedModelsDevTable(table *modelsDevTable) {
	if table == nil {
		return
	}
	raw, err := json.Marshal(table)
	if err != nil {
		return
	}
	dir := modelsDevCacheDir()
	_ = os.MkdirAll(dir, 0o755)
	tmp := filepath.Join(dir, "models-dev.json.tmp")
	if os.WriteFile(tmp, raw, 0o644) == nil {
		_ = os.Rename(tmp, modelsDevTableCachePath())
	}
}

func cachedModelsDevProviders() *modelsDevProviders {
	raw, err := os.ReadFile(modelsDevProvidersCachePath())
	if err != nil {
		return nil
	}
	var providers modelsDevProviders
	if json.Unmarshal(raw, &providers) != nil || providers.Providers == nil {
		return nil
	}
	return &providers
}

func writeCachedModelsDevProviders(providers *modelsDevProviders) {
	if providers == nil {
		return
	}
	raw, err := json.Marshal(providers)
	if err != nil {
		return
	}
	dir := modelsDevCacheDir()
	_ = os.MkdirAll(dir, 0o755)
	tmp := filepath.Join(dir, "models-dev-providers.json.tmp")
	if os.WriteFile(tmp, raw, 0o644) == nil {
		_ = os.Rename(tmp, modelsDevProvidersCachePath())
	}
}

func parseModelsDevModel(id string, spec map[string]any, providerID string) *modelsDevModel {
	record := &modelsDevModel{ID: id, Provider: providerID}
	if name, _ := spec["name"].(string); name != "" {
		n := name
		record.Name = &n
	}
	record.ContextWindow = modelsDevUint(spec, "context_window", "contextWindow")
	record.OutputLimit = modelsDevUint(spec, "output_limit", "outputLimit", "max_output_tokens")
	record.InputModalities = modelsDevModalities(spec, "input_modalities", "input")
	record.OutputModalities = modelsDevModalities(spec, "output_modalities", "output")
	return record
}

func modelsDevUint(spec map[string]any, keys ...string) *uint64 {
	for _, key := range keys {
		switch value := spec[key].(type) {
		case float64:
			if value > 0 {
				n := uint64(value)
				return &n
			}
		case int:
			if value > 0 {
				n := uint64(value)
				return &n
			}
		case int64:
			if value > 0 {
				n := uint64(value)
				return &n
			}
		case json.Number:
			if n, err := value.Int64(); err == nil && n > 0 {
				u := uint64(n)
				return &u
			}
		}
	}
	if limit, ok := spec["limit"].(map[string]any); ok {
		if n := modelsDevUint(limit, "context", "output"); n != nil {
			_ = n
		}
		if context, ok := limit["context"].(float64); ok && context > 0 {
			for _, key := range keys {
				if key == "context_window" {
					n := uint64(context)
					return &n
				}
			}
		}
	}
	return nil
}

func modelsDevModalities(spec map[string]any, keys ...string) []string {
	for _, key := range keys {
		if values, ok := spec[key].([]any); ok {
			var out []string
			for _, value := range values {
				if text, ok := value.(string); ok && text != "" {
					out = append(out, text)
				}
			}
			if len(out) > 0 {
				return out
			}
		}
	}
	if modalities, ok := spec["modalities"].(map[string]any); ok {
		for _, key := range []string{"input", "output"} {
			_ = key
		}
		if input, ok := modalities["input"].([]any); ok && len(keys) > 0 && keys[0] != "output_modalities" && keys[0] != "output" {
			var out []string
			for _, value := range input {
				if text, ok := value.(string); ok && text != "" {
					out = append(out, text)
				}
			}
			if len(out) > 0 {
				return out
			}
		}
		if output, ok := modalities["output"].([]any); ok {
			isOutput := false
			for _, key := range keys {
				if key == "output_modalities" || key == "output" {
					isOutput = true
				}
			}
			if isOutput {
				var out []string
				for _, value := range output {
					if text, ok := value.(string); ok && text != "" {
						out = append(out, text)
					}
				}
				if len(out) > 0 {
					return out
				}
			}
		}
	}
	return nil
}

func parseModelsDevCatalog(body string) (*modelsDevTable, error) {
	var document map[string]any
	if err := json.Unmarshal([]byte(body), &document); err != nil {
		return nil, err
	}
	now := time.Now().UnixMilli()
	models := map[string][]modelsDevModel{}
	for providerID, entry := range document {
		entryMap, ok := entry.(map[string]any)
		if !ok {
			continue
		}
		specs, ok := entryMap["models"].(map[string]any)
		if !ok {
			continue
		}
		for id, specRaw := range specs {
			spec, ok := specRaw.(map[string]any)
			if !ok {
				continue
			}
			record := parseModelsDevModel(id, spec, providerID)
			key := normalizeModelID(record.ID)
			models[key] = append(models[key], *record)
		}
	}
	return &modelsDevTable{FetchedAt: now, Models: models}, nil
}

func parseModelsDevProviders(body string) (*modelsDevProviders, error) {
	var document map[string]any
	if err := json.Unmarshal([]byte(body), &document); err != nil {
		return nil, err
	}
	now := time.Now().UnixMilli()
	providers := map[string]modelsDevProvider{}
	for key, entryRaw := range document {
		entry, ok := entryRaw.(map[string]any)
		if !ok {
			continue
		}
		id, _ := entry["id"].(string)
		if id == "" {
			id = key
		}
		specs, ok := entry["models"].(map[string]any)
		if !ok {
			continue
		}
		var models []modelsDevModel
		for modelID, specRaw := range specs {
			spec, ok := specRaw.(map[string]any)
			if !ok {
				continue
			}
			models = append(models, *parseModelsDevModel(modelID, spec, id))
		}
		if len(models) == 0 {
			continue
		}
		name, _ := entry["name"].(string)
		if name == "" {
			name = id
		}
		var api, npm *string
		if text, _ := entry["api"].(string); text != "" {
			c := text
			api = &c
		}
		if text, _ := entry["npm"].(string); text != "" {
			c := text
			npm = &c
		}
		providers[id] = modelsDevProvider{ID: id, Name: name, API: api, NPM: npm, Models: models}
	}
	sortedIDs := make([]string, 0, len(providers))
	for id := range providers {
		sortedIDs = append(sortedIDs, id)
	}
	sort.Slice(sortedIDs, func(i, j int) bool {
		left, right := providers[sortedIDs[i]], providers[sortedIDs[j]]
		lname, rname := strings.ToLower(left.Name), strings.ToLower(right.Name)
		if lname != rname {
			return lname < rname
		}
		return sortedIDs[i] < sortedIDs[j]
	})
	return &modelsDevProviders{FetchedAt: now, Providers: providers, SortedIDs: sortedIDs}, nil
}

func fetchModelsDevDocument(source settingsSource) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 40*time.Second)
	defer cancel()
	var result struct {
		Catalog json.RawMessage `json:"catalog"`
	}
	if err := settingsRequest(ctx, source, map[string]any{"type": "fetchModelsDevCatalog"}, "modelsDevCatalog", &result); err != nil {
		return "", err
	}
	if len(result.Catalog) == 0 {
		return "", io.ErrUnexpectedEOF
	}
	return string(result.Catalog), nil
}

func fetchModelsDevCatalog(source settingsSource) (*modelsDevTable, error) {
	body, err := fetchModelsDevDocument(source)
	if err != nil {
		return nil, err
	}
	table, err := parseModelsDevCatalog(body)
	if err != nil {
		return nil, err
	}
	writeCachedModelsDevTable(table)
	return table, nil
}

func fetchModelsDevCatalogWithFallback(source settingsSource, session *modelsDevTable) *modelsDevTable {
	if session != nil && session.age() < modelsDevReuseWindow {
		return session
	}
	if session == nil {
		if cached := cachedModelsDevTable(); cached != nil {
			go func() { _, _ = fetchModelsDevDocument(source) }()
			return cached
		}
	}
	if table, err := fetchModelsDevCatalog(source); err == nil {
		return table
	}
	if session != nil {
		return session
	}
	return cachedModelsDevTable()
}

func fetchModelsDevProvidersWithFallback(source settingsSource, session *modelsDevProviders) (*modelsDevProviders, error) {
	if session != nil && session.age() < modelsDevReuseWindow {
		return session, nil
	}
	if session == nil {
		if cached := cachedModelsDevProviders(); cached != nil {
			go func() { _, _ = fetchModelsDevDocument(source) }()
			return cached, nil
		}
	}
	body, err := fetchModelsDevDocument(source)
	if err == nil {
		if providers, perr := parseModelsDevProviders(body); perr == nil {
			writeCachedModelsDevProviders(providers)
			return providers, nil
		}
	}
	if session != nil {
		return session, nil
	}
	if cached := cachedModelsDevProviders(); cached != nil {
		return cached, nil
	}
	if err != nil {
		return nil, err
	}
	return nil, io.ErrUnexpectedEOF
}

func (t *modelsDevTable) resolveMatches(id string, preferred *string) []modelsDevModel {
	if t == nil {
		return nil
	}
	group, ok := t.Models[normalizeModelID(id)]
	if !ok {
		return nil
	}
	var exact []modelsDevModel
	for _, model := range group {
		if model.ID == id {
			exact = append(exact, model)
		}
	}
	matches := group
	if len(exact) > 0 {
		matches = exact
	}
	if preferred != nil && *preferred != "" {
		var own []modelsDevModel
		for _, model := range matches {
			if model.Provider == *preferred {
				own = append(own, model)
			}
		}
		if len(own) > 0 {
			return own
		}
	}
	return matches
}

func modeString(values []modelsDevModel, pick func(modelsDevModel) *string) *string {
	counts := map[string]int{}
	order := []string{}
	for _, model := range values {
		if v := pick(model); v != nil && *v != "" {
			if _, ok := counts[*v]; !ok {
				order = append(order, *v)
			}
			counts[*v]++
		}
	}
	best, bestCount := "", -1
	for _, key := range order {
		if counts[key] >= bestCount {
			best, bestCount = key, counts[key]
		}
	}
	if bestCount < 0 {
		return nil
	}
	out := best
	return &out
}

func modeUint(values []modelsDevModel, pick func(modelsDevModel) *uint64) *uint64 {
	counts := map[uint64]int{}
	order := []uint64{}
	for _, model := range values {
		if v := pick(model); v != nil && *v > 0 {
			if _, ok := counts[*v]; !ok {
				order = append(order, *v)
			}
			counts[*v]++
		}
	}
	var best uint64
	bestCount := -1
	for _, key := range order {
		if counts[key] >= bestCount {
			best, bestCount = key, counts[key]
		}
	}
	if bestCount < 0 {
		return nil
	}
	out := best
	return &out
}

func modeStrings(values []modelsDevModel, pick func(modelsDevModel) []string) []string {
	counts := map[string]int{}
	serialized := map[string][]string{}
	order := []string{}
	keyOf := func(list []string) string {
		return strings.Join(list, "\x00")
	}
	for _, model := range values {
		list := pick(model)
		if len(list) == 0 {
			continue
		}
		key := keyOf(list)
		if _, ok := counts[key]; !ok {
			order = append(order, key)
			serialized[key] = append([]string{}, list...)
		}
		counts[key]++
	}
	best, bestCount := "", -1
	for _, key := range order {
		if counts[key] >= bestCount {
			best, bestCount = key, counts[key]
		}
	}
	if bestCount < 0 {
		return nil
	}
	return serialized[best]
}

func (t *modelsDevTable) resolveBasics(id string, preferred *string) recordBasics {
	matches := t.resolveMatches(id, preferred)
	return recordBasics{
		name:             modeString(matches, func(m modelsDevModel) *string { return m.Name }),
		contextWindow:    modeUint(matches, func(m modelsDevModel) *uint64 { return m.ContextWindow }),
		outputLimit:      modeUint(matches, func(m modelsDevModel) *uint64 { return m.OutputLimit }),
		inputModalities:  modeStrings(matches, func(m modelsDevModel) []string { return m.InputModalities }),
		outputModalities: modeStrings(matches, func(m modelsDevModel) []string { return m.OutputModalities }),
	}
}

func (t *modelsDevTable) resolveInputModalities(id string) []string {
	if t == nil {
		return nil
	}
	return modeStrings(t.resolveMatches(id, nil), func(m modelsDevModel) []string { return m.InputModalities })
}

func (t *modelsDevTable) resolveOutputModalities(id string) []string {
	if t == nil {
		return nil
	}
	return modeStrings(t.resolveMatches(id, nil), func(m modelsDevModel) []string { return m.OutputModalities })
}

func customModelEmitsImage(modelID string, recordedOutput []string, providerSlug string, table *modelsDevTable) bool {
	for _, mode := range recordedOutput {
		if mode == "image" {
			return true
		}
	}
	if table == nil {
		return false
	}
	slug := providerSlug
	for _, list := range [][]string{
		modeStrings(table.resolveMatches(modelID, &slug), func(m modelsDevModel) []string { return m.OutputModalities }),
		modeStrings(table.resolveMatches(modelID, nil), func(m modelsDevModel) []string { return m.OutputModalities }),
	} {
		for _, mode := range list {
			if mode == "image" {
				return true
			}
		}
	}
	return false
}
