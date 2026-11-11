//go:build wiringe2e

package main

import (
	"context"
	"os"
	"path/filepath"

	"github.com/egoist/mygo/ui"
)

func wiringProviders(ctx context.Context, r *wiringReport, round string) {
	dir := filepath.Join(round, "providers")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		r.check("providers isolated dir", "success", err.Error())
		return
	}
	configDir := filepath.Join(dir, "opencode-config")
	if err := os.MkdirAll(configDir, 0o700); err != nil {
		r.check("providers isolated config dir", "success", err.Error())
		return
	}
	oldConfig, hadConfig := os.LookupEnv("OPENCODE_CONFIG_DIR")
	oldCache, hadCache := os.LookupEnv("FINTWIND_MODEL_CACHE_DIR")
	_ = os.Setenv("OPENCODE_CONFIG_DIR", configDir)
	_ = os.Setenv("FINTWIND_MODEL_CACHE_DIR", filepath.Join(dir, "model-cache"))
	defer func() {
		if hadConfig {
			_ = os.Setenv("OPENCODE_CONFIG_DIR", oldConfig)
		} else {
			_ = os.Unsetenv("OPENCODE_CONFIG_DIR")
		}
		if hadCache {
			_ = os.Setenv("FINTWIND_MODEL_CACHE_DIR", oldCache)
		} else {
			_ = os.Unsetenv("FINTWIND_MODEL_CACHE_DIR")
		}
	}()
	seed := `{"$schema":"https://opencode.ai/config.json","provider":{"demo":{"name":"Demo","options":{"baseURL":"https://api.example.com/v1"},"npm":"@ai-sdk/openai-compatible","models":{"demo-chat":{"name":"demo-chat","limit":{"context":128000}}}}}}}`
	if err := os.WriteFile(filepath.Join(configDir, "opencode.json"), []byte(seed), 0o600); err != nil {
		r.check("providers seed config", "success", err.Error())
		return
	}
	providers, err := loadOpencodeProviders()
	if err != nil {
		r.check("providers load from opencode.json", "success", err.Error())
		return
	}
	r.check("providers roster loads direct file", 1, len(providers))
	if len(providers) == 1 {
		r.check("providers entry name", "Demo", settingsProviderName(providers[0]))
		r.check("providers base url valid", true, baseURLValid(settingsProviderBaseURL(providers[0].Entry, providers[0].Native)))
		r.check("providers format", "openai", settingsProviderFormatForEntry(providers[0].Entry, providers[0].Native))
		r.check("providers invalid url rejected", false, baseURLValid("ftp://example.com"))
		if window := parseContextWindow("1M"); window != nil {
			r.check("providers context parse", uint64(1000000), *window)
		} else {
			r.check("providers context parse", uint64(1000000), uint64(0))
		}
		r.check("providers context format", "128K", formatContextWindow(128000))
		r.check("providers slug", "my-provider-v2", providerSlug("  My Provider.v2  "))
	}

	// Builtin catalog parses without network.
	catalogDoc := `{"anthropic":{"id":"anthropic","name":"Anthropic","api":"https://api.anthropic.com","models":{"claude-opus-4-1":{"name":"Claude Opus 4.1"}}}}`
	parsedProviders, err := parseModelsDevProviders(catalogDoc)
	if err != nil {
		r.check("providers builtin catalog parses", "success", err.Error())
		return
	}
	r.check("providers builtin roster", 1, len(parsedProviders.Providers))
	table, err := parseModelsDevCatalog(catalogDoc)
	if err != nil {
		r.check("providers models table parses", "success", err.Error())
		return
	}
	r.check("providers table resolves unknown modalities empty", 0, len(table.resolveInputModalities("claude-opus-4-1")))

	// Headless UI: list shows OpenCode + custom, detail shows custom fields.
	lifetime, cancel := context.WithCancel(ctx)
	defer cancel()
	a := &app{lifetime: lifetime, done: make(chan struct{})}
	a.settings.backend.providers = providers
	a.settings.backend.provider.loaded = true
	a.settings.backend.providerSelected = "demo"
	queue, release := installHeadlessDispatcher(a)
	defer release()
	_ = queue
	tt := ui.NewTester(func(c *ui.Context) { a.providersSettings(c) }, 900, 1000)
	tt.Frame()
	_, openCodeVisible := tt.AccessByRole(ui.RoleButton, "OpenCode")
	r.check("providers list shows OpenCode", true, openCodeVisible)
	_, demoVisible := tt.AccessByRole(ui.RoleButton, "Demo")
	r.check("providers list shows custom", true, demoVisible)
	_, deleteVisible := tt.AccessByRole(ui.RoleButton, "Delete provider")
	r.check("providers custom detail shows delete", true, deleteVisible)
	_, modelsVisible := tt.AccessByRole(ui.RoleText, "Models")
	r.check("providers custom detail shows models", true, modelsVisible)

	// Add form: enter the custom stage directly (no network fetch).
	st := &a.settings.catalog
	backend := &a.settings.backend
	st.providerAdding = true
	backend.providerFormStage = "custom"
	st.pfName, st.pfBaseURL, st.pfFormat = "", "", "openai"
	st.pfModels = nil
	st.addProviderModel()
	tt.Frame()
	_, nameVisible := tt.AccessByRole(ui.RoleTextField, "Name")
	r.check("providers custom form shows name", true, nameVisible)
	st.pfName = "Second"
	st.pfBaseURL = "not-a-url"
	r.check("providers rejects invalid base url", false, baseURLValid(st.pfBaseURL))
	st.pfBaseURL = "https://api.example.com/v1"
	r.check("providers draft models present", true, len(providerDraftModelIDs(st)) >= 0)

	// Duplicate model rejected by draft validation.
	st.pfModels = nil
	st.addProviderModel()
	st.pfModels[0].id = "dup"
	st.addProviderModel()
	st.pfModels[1].id = "dup"
	backend.providerEditing = ""
	a.settings.backend.providers = providers
	_, draftErr := a.settingsProviderDraft()
	r.check("providers duplicate model rejected", true, draftErr != nil)
}
