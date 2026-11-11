package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"github.com/egoist/mygo/ui"
)

const settingsLocalEnvironment = "00000000-0000-0000-0000-000000000000"

type settingsSource struct {
	client    *daemon.Client
	identity  daemon.Identity
	directory string
}

type settingsOperation struct {
	loading, loaded, writing, uncertain bool
	err, message                        string
}

type settingsBackendState struct {
	source                            settingsSource
	generation                        uint64
	providers                         []settingsProvider
	providerRevision                  uint64
	provider                          settingsOperation
	providerSelected, providerEditing string
	providerFormatTouched             bool
	providerDraftGeneration           uint64
	providerList                      ui.ListState
	providerRenaming                  bool
	providerRename                    string
	providerKeyRevealed               bool
	providerKeyDraft                  string
	providerModelEditor               *providerModelEditorState
	providerConnectivity              map[string]providerConnectivityState
	providerLatency                   map[string]providerLatencyState
	providerBuiltinCheck              *providerBuiltinCheckState
	providerBuiltinCheckID            string
	builtinProviders                  *modelsDevProviders
	builtinLoading                    bool
	builtinGeneration                 uint64
	modelsTable                       *modelsDevTable
	modelsTableLoading                bool
	providerFormStage                 string
	providerFormBuiltin               string
	providerFormSearch                string
	providerFormConnectivity          *providerConnectivityState
	providerFormFetching              bool
	providerFormFetchGen              uint64
	providerFormProbeGen              uint64
	providerCommitGen                 uint64
	providerSaveInFlight              bool
	providerSavePending               bool
	providerLoadGen                   uint64
	skills                            []settingsSkill
	skill                             settingsOperation
	skillSelected                     string
	skillList                         ui.ListState
	skillTrashConfirm                 bool
	mcpServers                        []settingsMcpServer
	mcpRevision                       uint64
	mcp                               settingsOperation
	mcpSelected, mcpEditing           string
	mcpDraftGeneration                uint64
	mcpList                           ui.ListState
	mcpStatuses                       []settingsMcpStatus
	mcpStatusError                    string
	removeConfirm                     string
	settingsWrite                     bool
	settingsUncertain                 bool
	auth                              settingsAuthState
	authGeneration                    uint64
	oauthForm                         *settingsOAuthFormState
	selectedCredentialID              string
	integrations                      []settingsIntegration
	integration                       settingsOperation
	integrationRefreshPending         bool
	integrationSelected               string
	integrationKey                    string
	integrationShowKey                bool
	general                           settingsGeneralState
}

type providerModelEditorState struct {
	add   bool
	index int
	id    string
	context string
	input map[string]bool
	output map[string]bool
	packageName string
}

type providerConnectivityState struct {
	testing   bool
	reachable bool
	models    int
	latencyMs int64
	err       string
}

type providerLatencyState struct {
	testing bool
	latencyMs int64
	err     string
}

type providerBuiltinCheckState struct {
	testing bool
	models  int
	err     string
}

type settingsOAuthFormState struct {
	providerID, methodID string
	answers              map[string]string
	inputs               map[string]*string
	showSecret           map[string]*bool
}

type settingsProvider struct {
	ID      string         `json:"id"`
	Entry   map[string]any `json:"entry"`
	Enabled bool           `json:"enabled"`
	Native  bool           `json:"native"`
}

type settingsSkillInstall struct {
	Source    string `json:"source"`
	Dir       string `json:"dir"`
	SkillFile string `json:"skillFile"`
	Enabled   bool   `json:"enabled"`
}

type settingsSkill struct {
	Name            string                 `json:"name"`
	Description     string                 `json:"description"`
	Scope           string                 `json:"scope"`
	Project         string                 `json:"project"`
	Installs        []settingsSkillInstall `json:"installs"`
	Enabled         bool                   `json:"enabled"`
	AllowedTools    *string                `json:"allowedTools"`
	Body            string                 `json:"body"`
	SupportingFiles int                    `json:"supportingFiles"`
	TotalBytes      uint64                 `json:"totalBytes"`
	ModifiedAt      *uint64                `json:"modifiedAt"`
	Duplicates      int                    `json:"duplicates"`
	RowKey          uint64                 `json:"rowKey"`
}

func (s settingsSkill) key() string {
	if len(s.Installs) != 0 {
		return s.Installs[0].Dir
	}
	return fmt.Sprint(s.RowKey)
}

type settingsMcpOAuth struct {
	Mode         string `json:"mode"`
	ClientID     string `json:"clientId"`
	ClientSecret string `json:"clientSecret"`
	Scope        string `json:"scope"`
}

type settingsMcpServer struct {
	Name        string           `json:"name"`
	Kind        string           `json:"kind"`
	Command     []string         `json:"command"`
	URL         string           `json:"url"`
	Environment [][2]string      `json:"environment"`
	Headers     [][2]string      `json:"headers"`
	OAuth       settingsMcpOAuth `json:"oauth"`
	Enabled     bool             `json:"enabled"`
	Raw         map[string]any   `json:"raw"`
}

type settingsMcpStatus struct {
	Name   string `json:"name"`
	Status string `json:"status"`
	Error  string `json:"error"`
}

func (a *app) currentSettingsSource() settingsSource {
	source := settingsSource{client: a.client}
	if a.client != nil {
		source.identity = a.client.Identity()
	}
	if project := a.selectedProject(); project != nil {
		source.directory = project.Path
	} else {
		source.directory = a.projectlessRoot
	}
	return source
}

// Call on source changes before reading cached directories.
func (a *app) syncSettingsSource() {
	source := a.currentSettingsSource()
	st := &a.settings.backend
	if st.source == source {
		return
	}
	a.cancelSettingsOAuth()
	a.clearSettingsMcpSecrets()
	generation := st.generation + 1
	*st = settingsBackendState{source: source, generation: generation}
	a.cancelProviderAdd()
	a.settings.catalog.mcpAdding = false
	a.settingsLoaded, a.settings.loading = false, false
	a.settingsRaw = nil
}

func (a *app) settingsSourceMatches(source settingsSource, generation uint64) bool {
	return a.settings.backend.generation == generation && a.currentSettingsSource() == source
}

func settingsRequest(ctx context.Context, source settingsSource, command map[string]any, expected string, out any) error {
	if source.client == nil {
		return errors.New("Daemon is not connected")
	}
	payload, err := source.client.Request(ctx, command)
	if err != nil {
		return err
	}
	var envelope struct {
		Type string `json:"type"`
	}
	if json.Unmarshal(payload, &envelope) != nil || envelope.Type != expected {
		return errors.New("Unexpected daemon response; reload before retrying")
	}
	if out != nil {
		decoder := json.NewDecoder(bytes.NewReader(payload))
		decoder.UseNumber()
		return decoder.Decode(out)
	}
	return nil
}

func settingsCommand(kind string, source settingsSource) map[string]any {
	return map[string]any{"type": kind, "environmentId": settingsLocalEnvironment, "directory": source.directory}
}

func (a *app) loadSettingsCatalog(page string) {
	a.syncSettingsSource()
	if a.client == nil {
		return
	}
	switch page {
	case "providers":
		a.loadSettingsProviders()
	case "skills":
		a.loadSettingsSkills()
	case "mcpServers", "mcpMarket":
		a.loadSettingsMcp()
	}
}

func (a *app) loadSettingsProviders() {
	st := &a.settings.backend
	if st.provider.loading || st.provider.writing || st.providerSaveInFlight {
		return
	}
	st.provider.loading, st.provider.err = true, ""
	st.providerLoadGen++
	generation := st.providerLoadGen
	source, sourceGen := st.source, st.generation
	go func() {
		providers, err := loadOpencodeProviders()
		table := fetchModelsDevCatalogWithFallback(a.settings.backend.modelsTable)
		builtin, _ := fetchModelsDevProvidersWithFallback(a.settings.backend.builtinProviders)
		a.post(func() {
			if !a.settingsSourceMatches(source, sourceGen) || a.settings.backend.providerLoadGen != generation {
				return
			}
			st := &a.settings.backend
			st.provider.loading = false
			if err != nil {
				st.provider.err = tr("Could not load providers. ") + err.Error()
				return
			}
			if st.providerCommitGen != 0 {
				return
			}
			selectedMissing := st.providerSelected != "" && !isBuiltinSelection(st.providerSelected) && st.providerSelected != "opencode"
			if selectedMissing {
				found := false
				for _, provider := range providers {
					if provider.ID == st.providerSelected {
						found = true
						break
					}
				}
				if !found {
					st.providerSelected = ""
					st.providerAddingReset()
				}
			}
			st.providers = providers
			st.provider.loaded, st.provider.uncertain = true, false
			if table != nil {
				st.modelsTable = table
			}
			if builtin != nil {
				st.builtinProviders = builtin
			}
			a.refreshSettingsIntegrations()
		})
	}()
}

func isBuiltinSelection(id string) bool {
	return len(id) > 8 && id[:8] == "builtin:"
}

func builtinSelectionID(catalogID string) string {
	return "builtin:" + catalogID
}

func (st *settingsBackendState) providerAddingReset() {
	st.providerRenaming = false
	st.providerKeyRevealed = false
	st.removeConfirm = ""
	st.providerModelEditor = nil
	st.providerFormStage = ""
	st.providerFormBuiltin = ""
}

func (a *app) loadSettingsSkills() {
	st := &a.settings.backend
	if st.skill.loading || st.skill.writing {
		return
	}
	st.skill.loading, st.skill.err = true, ""
	source, generation := st.source, st.generation
	projects := make([][2]string, 0, len(a.projects))
	for _, project := range a.projects {
		if project.EnvironmentID == "" || project.EnvironmentID == settingsLocalEnvironment {
			projects = append(projects, [2]string{project.Name, project.Path})
		}
	}
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 45*time.Second)
		defer cancel()
		var body struct {
			Catalog struct {
				Skills []settingsSkill `json:"skills"`
			} `json:"catalog"`
		}
		err := settingsRequest(ctx, source, map[string]any{"type": "loadSkills", "projects": projects}, "skillsCatalog", &body)
		if err == nil && body.Catalog.Skills == nil {
			err = errors.New("Skill list is missing from the response")
		}
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.skill.loading = false
			if err != nil {
				st.skill.err = tr("Could not load skills. ") + err.Error()
				return
			}
			st.skills, st.skill.loaded, st.skill.uncertain = body.Catalog.Skills, true, false
		})
	}()
}

func (a *app) loadSettingsMcp() {
	st := &a.settings.backend
	if st.mcp.loading || st.mcp.writing {
		return
	}
	st.mcp.loading, st.mcp.err = true, ""
	source, generation := st.source, st.generation
	binary := a.settingsStr("opencode_binary_path", "opencode")
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
		defer cancel()
		var body struct {
			Servers  []settingsMcpServer `json:"servers"`
			Revision uint64              `json:"revision"`
		}
		err := settingsRequest(ctx, source, settingsCommand("loadMcpServers", source), "mcpServers", &body)
		if err == nil && body.Servers == nil {
			err = errors.New("MCP server list is missing from the response")
		}
		var statuses struct {
			Statuses []settingsMcpStatus `json:"statuses"`
		}
		var statusErr error
		if err == nil {
			statusErr = settingsRequest(ctx, source, map[string]any{"type": "listMcpServerStatuses", "binary": binary, "directory": source.directory}, "mcpServerStatuses", &statuses)
		}
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.mcp.loading = false
			if err != nil {
				st.mcp.err = tr("Could not load MCP servers. ") + err.Error()
				return
			}
			st.mcpServers, st.mcpRevision, st.mcp.loaded, st.mcp.uncertain = body.Servers, body.Revision, true, false
			st.mcpStatuses = statuses.Statuses
			st.mcpStatusError = ""
			if statusErr != nil {
				st.mcpStatusError = "Connection status unavailable. " + statusErr.Error()
			}
		})
	}()
}

// toggleSettingsSkill flips every copy of the skill keyed by its primary
// dir, mirroring GPUI toggle_skill_enabled: the switch answers immediately
// from the cached catalog, then SetSkillsEnabled confirms from disk.
func (a *app) toggleSettingsSkill(skill settingsSkill, enabled bool) {
	st := &a.settings.backend
	if st.skill.writing || st.skill.uncertain || !st.skill.loaded {
		return
	}
	dirs := make([]string, 0, len(skill.Installs))
	for _, install := range skill.Installs {
		dirs = append(dirs, install.Dir)
	}
	if len(dirs) == 0 {
		return
	}
	for i := range st.skills {
		if st.skills[i].key() != skill.key() {
			continue
		}
		st.skills[i].Enabled = enabled
		for j := range st.skills[i].Installs {
			st.skills[i].Installs[j].Enabled = enabled
		}
	}
	source, generation := st.source, st.generation
	st.skill.writing, st.skill.err, st.skill.message, st.skillTrashConfirm = true, "", "", false
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 45*time.Second)
		defer cancel()
		err := settingsRequest(ctx, source, map[string]any{"type": "setSkillsEnabled", "dirs": dirs, "enabled": enabled}, "ack", nil)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.skill.writing = false
			if err != nil {
				st.skill.err = strings.ReplaceAll(tr("Couldn't update the skill: %{error}"), "%{error}", err.Error())
				st.skill.uncertain = true
				return
			}
			a.loadSettingsSkills()
		})
	}()
}

// trashSettingsSkill removes every copy of the skill keyed by its primary
// dir, mirroring GPUI delete_skill: the row leaves the roster immediately,
// the trash runs in the background, then the library rescans from disk.
func (a *app) trashSettingsSkill(skill settingsSkill) {
	st := &a.settings.backend
	if st.skill.writing || st.skill.uncertain || !st.skill.loaded {
		return
	}
	dirs := make([]string, 0, len(skill.Installs))
	for _, install := range skill.Installs {
		dirs = append(dirs, install.Dir)
	}
	if len(dirs) == 0 {
		return
	}
	name := skill.Name
	kept := make([]settingsSkill, 0, len(st.skills))
	for _, item := range st.skills {
		if item.key() != skill.key() {
			kept = append(kept, item)
		}
	}
	st.skills = kept
	if st.skillSelected == skill.key() {
		st.skillSelected = ""
	}
	source, generation := st.source, st.generation
	st.skill.writing, st.skill.err, st.skill.message, st.skillTrashConfirm = true, "", "", false
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 45*time.Second)
		defer cancel()
		err := settingsRequest(ctx, source, map[string]any{"type": "trashSkills", "dirs": dirs}, "ack", nil)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.skill.writing = false
			if err != nil {
				st.skill.err = strings.ReplaceAll(tr("Couldn't delete the skill: %{error}"), "%{error}", err.Error())
				st.skill.uncertain = true
				return
			}
			st.skill.message = strings.ReplaceAll(tr("Moved \u201c%{name}\u201d to the Trash"), "%{name}", name)
			a.loadSettingsSkills()
		})
	}()
}

func (a *app) mutateSettingsSkill(skill settingsSkill, trash bool) {
	if trash {
		a.trashSettingsSkill(skill)
		return
	}
	a.toggleSettingsSkill(skill, !skill.Enabled)
}

func settingsCloneObject(value map[string]any) map[string]any {
	if value == nil {
		return map[string]any{}
	}
	data, _ := json.Marshal(value)
	result := map[string]any{}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	_ = decoder.Decode(&result)
	return result
}

func settingsObject(parent map[string]any, key string) map[string]any {
	if object, ok := parent[key].(map[string]any); ok {
		return object
	}
	object := map[string]any{}
	parent[key] = object
	return object
}

func settingsString(object map[string]any, key string) string {
	value, _ := object[key].(string)
	return value
}

func settingsOperationStatus(c *ui.Context, op settingsOperation, empty string) {
	text := empty
	if op.loading {
		text = tr("Loading...")
	} else if op.writing {
		text = tr("Saving...")
	} else if op.err != "" {
		text = op.err
	} else if op.message != "" {
		text = op.message
	}
	if text != "" {
		ui.Text(c, text).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
	}
}

func settingsMatches(query, text string) bool {
	return strings.Contains(strings.ToLower(text), strings.ToLower(strings.TrimSpace(query)))
}
