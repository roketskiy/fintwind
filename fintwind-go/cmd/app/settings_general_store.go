package main

import (
	"context"
	"net/url"
	"strconv"
	"strings"
	"time"
)

func settingsBool(values map[string]any, key string, fallback bool) bool {
	value, ok := values[key].(bool)
	if !ok {
		return fallback
	}
	return value
}

func (a *app) applyDaemonSettings(values map[string]any) {
	a.settingsRaw = values
	a.settings.networkProxy, _ = values["network_proxy_url"].(string)
	a.settings.scheduledTools = settingsBool(values, "scheduled_tools_enabled", false)
	a.settings.autoCompact = settingsBool(values, "auto_compact_enabled", true)
	a.settings.autoCompactThresh = strconv.FormatUint(a.runtimePreferences().AutoCompactThreshold, 10)
	a.settings.busySendSteer = a.preferenceString("busy_send_mode", "queue") == "steer"
	a.settings.busySendSel = choice(a.settings.busySendSteer, 1, 0)
	a.settings.mermaidVisual = settingsBool(a.settings.preferences.values, "mermaid_visualization_enabled", false)
}

type runtimeSettingsPreferences struct {
	BusySendSteer        bool
	ScheduledTools       bool
	MermaidVisualization bool
	AutoCompact          bool
	AutoCompactThreshold uint64
}

// runtimePreferences reads memory only. The runtime owner consumes this at startup and admission.
func (a *app) runtimePreferences() runtimeSettingsPreferences {
	threshold := a.settingsFloat("auto_compact_threshold_tokens", 500000)
	if threshold < 10000 {
		threshold = 10000
	}
	if threshold > 100000000 {
		threshold = 100000000
	}
	return runtimeSettingsPreferences{
		BusySendSteer:        a.preferenceString("busy_send_mode", "queue") == "steer",
		ScheduledTools:       settingsBool(a.settingsRaw, "scheduled_tools_enabled", false),
		MermaidVisualization: settingsBool(a.settings.preferences.values, "mermaid_visualization_enabled", false),
		AutoCompact:          settingsBool(a.settingsRaw, "auto_compact_enabled", true),
		AutoCompactThreshold: uint64(threshold),
	}
}

func (a *app) saveDaemonSetting(key string, value any) {
	allowed := key == "network_proxy_url" || key == "scheduled_tools_enabled" || key == "auto_compact_enabled" || key == "auto_compact_threshold_tokens" || key == "opencode_binary_path" || key == "general_workspace_path"
	if !allowed {
		a.settings.message = tr("This setting is not supported by the daemon")
		return
	}
	a.syncSettingsSource()
	st := &a.settings.backend
	if st.settingsWrite || st.settingsUncertain || a.client == nil {
		a.settings.message = tr("Reload settings before changing another value")
		return
	}
	source, generation := st.source, st.generation
	if a.settings.settingsWriteGate == nil {
		a.settings.settingsWriteGate = make(chan struct{}, 1)
	}
	gate := a.settings.settingsWriteGate
	st.settingsWrite = true
	a.settings.message = tr("Saving...")
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 30*time.Second)
		defer cancel()
		select {
		case gate <- struct{}{}:
			defer func() { <-gate }()
		case <-ctx.Done():
			a.post(func() {
				if a.settingsSourceMatches(source, generation) {
					a.settings.backend.settingsWrite = false
					a.settings.message = tr("Settings write cancelled")
				}
			})
			return
		}
		var body struct {
			Settings map[string]any `json:"settings"`
		}
		err := settingsRequest(ctx, source, map[string]any{"type": "updateSetting", "key": key, "value": value}, "settings", &body)
		if err == nil && body.Settings == nil {
			a.post(func() {
				if a.settingsSourceMatches(source, generation) {
					a.settings.backend.settingsWrite = false
					a.settings.message = tr("Invalid settings response")
				}
			})
			return
		}
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.settingsWrite = false
			if err != nil {
				st.settingsUncertain = true
				a.settings.message = tr("Not confirmed. Reload before retrying. ") + err.Error()
				a.applyDaemonSettings(a.settingsRaw)
				return
			}
			if key == "general_workspace_path" {
				if path, ok := value.(string); ok {
					a.projectlessRoot = path
				}
				a.syncSettingsSource()
			}
			a.applyDaemonSettings(body.Settings)
			a.settings.backend.general.pendingKey, a.settings.backend.general.pendingPath = "", ""
			if key == "opencode_binary_path" {
				a.modelProbe = nil
				resetModelProbeRetry()
			}
			a.settingsLoaded = true
			a.settings.message = tr("Saved")
		})
	}()
}

func (a *app) saveSettingsProxy() {
	proxy := strings.TrimSpace(a.settings.networkProxy)
	if proxy != "" {
		parsed, err := url.Parse(proxy)
		if err != nil || parsed.Hostname() == "" || parsed.Port() == "" || (parsed.Scheme != "http" && parsed.Scheme != "https") || parsed.User != nil || (parsed.Path != "" && parsed.Path != "/") || parsed.RawQuery != "" || parsed.Fragment != "" {
			a.settings.message = tr("Use an HTTP or HTTPS proxy with host and port, without credentials")
			return
		}
		port, err := strconv.ParseUint(parsed.Port(), 10, 16)
		if err != nil || port == 0 {
			a.settings.message = tr("Proxy port must be between 1 and 65535")
			return
		}
	}
	a.saveDaemonSetting("network_proxy_url", proxy)
}

func (a *app) saveSettingsThreshold() {
	value, err := strconv.ParseUint(strings.TrimSpace(a.settings.autoCompactThresh), 10, 64)
	if err != nil || value < 10000 || value > 100000000 {
		a.settings.message = tr("Threshold must be between 10000 and 100000000 tokens")
		return
	}
	a.saveDaemonSetting("auto_compact_threshold_tokens", value)
}

func (a *app) reloadGeneralSettings() {
	if a.settings.backend.settingsWrite {
		return
	}
	a.settingsLoaded = false
	a.settings.backend.settingsUncertain = false
	a.ensureSettingsLoaded()
}
