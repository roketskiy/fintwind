package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode"
)

func settingsProviderPackage(format string, native bool) string {
	name := map[string]string{"openai": "openai-compatible", "openai-responses": "openai", "anthropic": "anthropic", "google": "google"}[format]
	if name == "" {
		return ""
	}
	if native {
		return "@opencode/ai/providers/" + name
	}
	return "@ai-sdk/" + name
}

func settingsProviderFormat(packageName string) string {
	packageName = strings.TrimPrefix(packageName, "aisdk:")
	for _, native := range []bool{false, true} {
		for _, format := range []string{"openai", "openai-responses", "anthropic", "google"} {
			if settingsProviderPackage(format, native) == packageName {
				return format
			}
		}
	}
	if strings.HasSuffix(packageName, "/openai/responses") {
		return "openai-responses"
	}
	if strings.HasSuffix(packageName, "/openai/chat") {
		return "openai"
	}
	return ""
}

func settingsProviderName(provider settingsProvider) string {
	if name := settingsString(provider.Entry, "name"); name != "" {
		return name
	}
	return provider.ID
}

func (a *app) beginSettingsProvider(provider *settingsProvider) {
	a.cancelProviderAdd()
	st := &a.settings.catalog
	st.pfName, st.pfBaseURL, st.pfFormat = "", "", "openai"
	st.pfModels, st.pfModelsReady = nil, true
	a.settings.backend.providerFormatTouched = false
	if provider != nil {
		a.settings.backend.providerEditing = provider.ID
		a.settings.backend.providerSelected = provider.ID
		st.pfName = settingsProviderName(*provider)
		optionsKey, packageKey := "options", "npm"
		if provider.Native {
			optionsKey, packageKey = "settings", "package"
		}
		options, _ := provider.Entry[optionsKey].(map[string]any)
		st.pfBaseURL = settingsString(options, "baseURL")
		st.pfFormat = settingsProviderFormat(settingsString(provider.Entry, packageKey))
		if st.pfFormat == "" {
			st.pfFormat = settingsString(provider.Entry, packageKey)
			if st.pfFormat == "" {
				st.pfFormat = "Managed by OpenCode"
			}
		}
		models, _ := provider.Entry["models"].(map[string]any)
		ids := make([]string, 0, len(models))
		for id := range models {
			ids = append(ids, id)
		}
		sort.Strings(ids)
		for _, id := range ids {
			st.addProviderModel()
			draft := st.pfModels[len(st.pfModels)-1]
			draft.id = id
			spec, _ := models[id].(map[string]any)
			if limit, ok := spec["limit"].(map[string]any); ok {
				if size, ok := limit["context"].(float64); ok {
					draft.context = strconv.FormatFloat(size, 'f', 0, 64)
				}
				if size, ok := limit["context"].(json.Number); ok {
					draft.context = size.String()
				}
			}
			key := "modalities"
			if provider.Native {
				key = "capabilities"
			}
			if modalities, ok := spec[key].(map[string]any); ok {
				for name, target := range map[string]map[string]bool{"input": draft.input, "output": draft.output} {
					if values, ok := modalities[name].([]any); ok {
						clear(target)
						for _, value := range values {
							if text, ok := value.(string); ok {
								target[text] = true
							}
						}
					}
				}
			}
		}
	} else {
		st.addProviderModel()
	}
	st.providerAdding = true
}

func settingsProviderSlug(name string) string {
	var result strings.Builder
	for _, r := range strings.ToLower(strings.TrimSpace(name)) {
		if unicode.IsLetter(r) || unicode.IsDigit(r) || r == '_' || r == '-' {
			result.WriteRune(r)
		} else if r == ' ' {
			result.WriteByte('-')
		}
	}
	return strings.Trim(result.String(), "-")
}

func settingsModalities(values map[string]bool) []string {
	result := make([]string, 0, len(values))
	for value, enabled := range values {
		if enabled {
			result = append(result, value)
		}
	}
	sort.Strings(result)
	return result
}

func (a *app) settingsProviderDraft() (settingsProvider, error) {
	form, backend := &a.settings.catalog, &a.settings.backend
	name := strings.TrimSpace(form.pfName)
	if name == "" {
		return settingsProvider{}, errors.New("Enter a provider name")
	}
	parsed, err := url.Parse(strings.TrimSpace(form.pfBaseURL))
	if err != nil || parsed.Host == "" || (parsed.Scheme != "http" && parsed.Scheme != "https") || parsed.User != nil {
		return settingsProvider{}, errors.New("Enter an HTTP or HTTPS base URL without credentials")
	}
	provider := settingsProvider{ID: settingsProviderSlug(name), Entry: map[string]any{}, Enabled: true}
	for _, saved := range backend.providers {
		if saved.ID == backend.providerEditing {
			provider = saved
			provider.Entry = settingsCloneObject(saved.Entry)
			break
		}
	}
	if provider.ID == "" {
		return provider, errors.New("Enter a provider name with letters or digits")
	}
	if backend.providerEditing == "" {
		for _, saved := range backend.providers {
			if saved.ID == provider.ID {
				return provider, errors.New("A provider with this ID already exists")
			}
		}
	}
	optionsKey, packageKey, modalitiesKey := "options", "npm", "modalities"
	if provider.Native {
		optionsKey, packageKey, modalitiesKey = "settings", "package", "capabilities"
	}
	provider.Entry["name"] = name
	settingsObject(provider.Entry, optionsKey)["baseURL"] = strings.TrimSpace(form.pfBaseURL)
	if backend.providerEditing == "" || backend.providerFormatTouched {
		packageName := settingsProviderPackage(form.pfFormat, provider.Native)
		if packageName == "" {
			return provider, errors.New("Choose an API format")
		}
		provider.Entry[packageKey] = packageName
	}
	oldModels, _ := provider.Entry["models"].(map[string]any)
	models := map[string]any{}
	for _, draft := range form.pfModels {
		id := strings.TrimSpace(draft.id)
		if id == "" {
			continue
		}
		if _, exists := models[id]; exists {
			return provider, fmt.Errorf("Duplicate model ID: %s", id)
		}
		old, _ := oldModels[id].(map[string]any)
		spec := settingsCloneObject(old)
		if spec["name"] == nil {
			spec["name"] = id
		}
		if strings.TrimSpace(draft.context) != "" {
			size, err := strconv.ParseUint(strings.TrimSpace(draft.context), 10, 64)
			if err != nil || size == 0 {
				return provider, errors.New("Context window must be a positive integer")
			}
			limit := settingsObject(spec, "limit")
			limit["context"] = size
			if limit["output"] == nil {
				limit["output"] = 32000
			}
		} else if limit, ok := spec["limit"].(map[string]any); ok {
			delete(limit, "context")
		}
		modalities := settingsObject(spec, modalitiesKey)
		for key, values := range map[string]map[string]bool{"input": draft.input, "output": draft.output} {
			if list := settingsModalities(values); len(list) > 0 {
				modalities[key] = list
			} else {
				delete(modalities, key)
			}
		}
		models[id] = spec
	}
	provider.Entry["models"] = models
	return provider, nil
}

func (a *app) saveSettingsProvider() {
	st := &a.settings.backend
	if !st.provider.loaded || st.provider.writing || st.provider.uncertain {
		return
	}
	provider, err := a.settingsProviderDraft()
	if err != nil {
		st.provider.err = err.Error()
		return
	}
	roster := append([]settingsProvider{}, st.providers...)
	replaced := false
	for i := range roster {
		if roster[i].ID == provider.ID {
			roster[i], replaced = provider, true
			break
		}
	}
	if !replaced {
		roster = append(roster, provider)
	}
	key := strings.TrimSpace(a.settings.catalog.pfAPIKey)
	a.commitSettingsProviders(roster, provider.ID, key)
}

func (a *app) commitSettingsProviders(roster []settingsProvider, selected, key string) {
	st := &a.settings.backend
	if st.provider.writing || !st.provider.loaded || st.provider.uncertain {
		return
	}
	source, generation, revision := st.source, st.generation, st.providerRevision
	draftGeneration, wasAdding, previousSelected := st.providerDraftGeneration, a.settings.catalog.providerAdding, st.providerSelected
	binary := a.settingsStr("opencode_binary_path", "opencode")
	st.provider.writing, st.provider.err, st.provider.message = true, "", ""
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
		defer cancel()
		var err error
		keySaved := false
		if key != "" {
			err = settingsRequest(ctx, source, map[string]any{"type": "storeCustomProviderKey", "binary": binary, "directory": source.directory, "providerId": selected, "key": key}, "ack", nil)
			keySaved = err == nil
		}
		var body struct {
			Providers []settingsProvider `json:"providers"`
			Revision  uint64             `json:"revision"`
		}
		if err == nil {
			command := settingsCommand("saveCustomProviders", source)
			command["expectedRevision"], command["providers"] = revision, roster
			err = settingsRequest(ctx, source, command, "customProviders", &body)
			if err == nil && body.Providers == nil {
				err = errors.New("Saved provider list is missing from the response")
			}
		}
		key = ""
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.provider.writing = false
			if err != nil {
				st.provider.err = tr("Save failed. Reload before retrying. ") + err.Error()
				if keySaved {
					st.provider.err = tr("API key saved; provider configuration not confirmed. Reload before retrying. ") + err.Error()
					if st.providerDraftGeneration == draftGeneration {
						a.settings.catalog.pfAPIKey = ""
					}
				}
				st.provider.uncertain = true
				return
			}
			st.providers, st.providerRevision = body.Providers, body.Revision
			if st.providerSelected == previousSelected {
				st.providerSelected = selected
			}
			if wasAdding && st.providerDraftGeneration == draftGeneration {
				a.cancelProviderAdd()
			}
			st.provider.message = tr("Saved")
		})
	}()
}

func (a *app) probeSettingsProvider(id string) {
	st := &a.settings.backend
	if st.provider.writing || st.provider.loading {
		return
	}
	source, generation := st.source, st.generation
	st.provider.writing, st.provider.err, st.provider.message = true, "", ""
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
		defer cancel()
		command := settingsCommand("probeCustomProvider", source)
		command["providerId"] = id
		var body struct {
			Models []struct {
				ID string `json:"id"`
			} `json:"models"`
			Latency uint64 `json:"latencyMs"`
		}
		err := settingsRequest(ctx, source, command, "customProviderProbed", &body)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.provider.writing = false
			if err != nil {
				st.provider.err = tr("Probe failed. ") + err.Error()
				return
			}
			st.provider.message = fmt.Sprintf(tr("%d models · %d ms"), len(body.Models), body.Latency)
		})
	}()
}
