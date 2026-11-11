package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/action"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

type settingsGeneralState struct {
	operation               settingsOperation
	choosing                bool
	pendingKey, pendingPath string
	update                  settingsOpenCodeUpdate
	cleanupPeriod           string
	cleanupPreview          *settingsCleanupPreview
	// cleanupProgress drives the "deleting N of M" caption while the
	// confirm loop removes sessions one daemon call at a time.
	cleanupTotal, cleanupDone int
}

type settingsOpenCodeUpdate struct {
	Path            string `json:"path"`
	CurrentVersion  string `json:"currentVersion"`
	LatestVersion   string `json:"latestVersion"`
	UpdateAvailable bool   `json:"updateAvailable"`
	CheckError      string `json:"checkError"`
}

type settingsCleanupPreview struct {
	Before     uint64   `json:"before"`
	SessionIDs []string `json:"session_ids"`
}

func (a *app) chooseSettingsPath(key string, directory bool) {
	a.syncSettingsSource()
	st := &a.settings.backend
	if st.general.choosing || st.settingsWrite || st.settingsUncertain || st.general.operation.writing {
		return
	}
	st.general.choosing = true
	source, generation, window := st.source, st.generation, a.win
	go func() {
		options := mygo.OpenDialogOptions{Parent: window, Directory: directory, Title: choice(directory, "Choose general workspace", "Choose OpenCode executable")}
		if !directory {
			options.Filters = []mygo.FileFilter{{Name: "Executable", Extensions: []string{"exe", "cmd", "bat"}}}
		}
		paths, err := mygo.Dialog.Open(options)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.general.choosing = false
			if err != nil {
				a.settings.message = tr("Could not open the file picker")
				return
			}
			if len(paths) == 0 {
				return
			}
			st.general.pendingKey, st.general.pendingPath = key, paths[0]
		})
	}()
}

// settingsPathConfirmation is the inline apply prompt for one picked path,
// rendered inside the card that owns the setting. key filters which pending
// choice this card shows.
func (a *app) settingsPathConfirmation(c *ui.Context, key string) {
	st := &a.settings.backend
	if st.general.pendingPath == "" || st.general.pendingKey != key {
		return
	}
	ui.Column(c).FillWidth().Margin(12, 0, 0, 0).Gap(8).Children(func() {
		ui.Text(c, choice(st.general.pendingKey == "general_workspace_path", tr("Use this directory for new general-workspace tasks? Existing files will not be moved."), tr("Use this executable for future OpenCode launches?"))).FontSize(12).TextColor(palette.text).Wrap()
		ui.Text(c, st.general.pendingPath).Font(monoFont()).FontSize(11).TextColor(palette.textSecondary).Wrap()
		ui.Row(c).Gap(8).Children(func() {
			c.Key("settings-path-cancel")
			cancel := action.Button(c, tr("Cancel"), action.ButtonProps{Variant: action.Text, Size: theme.Small})
			cancel.Disabled(st.settingsWrite).OnClick(func() { st.general.pendingPath, st.general.pendingKey = "", "" })
			c.Key("settings-path-confirm")
			apply := action.Button(c, tr("Apply"), action.ButtonProps{Variant: action.Default, Size: theme.Small})
			apply.Disabled(st.settingsWrite || st.settingsUncertain).OnClick(func() { a.saveDaemonSetting(st.general.pendingKey, st.general.pendingPath) })
		})
	})
}

func (a *app) manageSettingsOpenCode(action string) {
	a.syncSettingsSource()
	st := &a.settings.backend
	if st.general.operation.writing || st.general.operation.uncertain || st.settingsWrite {
		return
	}
	source, generation := st.source, st.generation
	command := map[string]any{"type": "checkOpenCodeUpdate"}
	expected := "openCodeUpdateStatus"
	if action == "update" {
		if !st.general.update.UpdateAvailable || st.general.update.Path == "" || st.general.update.LatestVersion == "" {
			return
		}
		command["type"], command["path"], command["version"], expected = "updateOpenCode", st.general.update.Path, st.general.update.LatestVersion, "ack"
	} else if action == "install" {
		command["type"], expected = "installOpenCode", "openCodeInstalled"
	}
	st.general.operation.writing, st.general.operation.err, st.general.operation.message = true, "", ""
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 180*time.Second)
		defer cancel()
		var update settingsOpenCodeUpdate
		var installed struct {
			Settings map[string]any `json:"settings"`
		}
		var out any
		if action == "check" {
			out = &update
		}
		if action == "install" {
			out = &installed
		}
		err := settingsRequest(ctx, source, command, expected, out)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.general.operation.writing = false
			if err != nil {
				st.general.operation.err = tr("OpenCode operation failed. ") + err.Error()
				if action != "check" {
					st.general.operation.uncertain = true
				}
				return
			}
			if action == "check" {
				st.general.update, st.general.operation.loaded = update, true
				if update.CheckError != "" {
					st.general.operation.err = update.CheckError
				}
				return
			}
			if action == "install" {
				a.applyDaemonSettings(installed.Settings)
			}
			st.general.operation.message = tr("OpenCode updated")
			a.modelProbe = nil
			resetModelProbeRetry()
		})
	}()
}

func (a *app) settingsOpenCodeControl(c *ui.Context) {
	st := &a.settings.backend.general
	settingsButton(c, "settings-opencode-check", tr("Check for updates")).Disabled(st.operation.writing).OnClick(func() { st.operation.uncertain = false; a.manageSettingsOpenCode("check") })
	if st.operation.loaded && st.update.Path == "" {
		settingsButton(c, "settings-opencode-install", tr("Install")).Disabled(st.operation.writing || st.operation.uncertain).OnClick(func() { a.manageSettingsOpenCode("install") })
	} else if st.update.UpdateAvailable {
		settingsButton(c, "settings-opencode-update", tr("Update to ")+st.update.LatestVersion).Disabled(st.operation.writing || st.operation.uncertain).OnClick(func() { a.manageSettingsOpenCode("update") })
	}
}

// resetSettingsDaemonKey clears one daemon setting — the daemon treats a
// null value as removal — the "use automatic detection" and "restore
// default" paths.
func (a *app) resetSettingsDaemonKey(key string) {
	if key == "general_workspace_path" {
		a.projectlessRoot = ""
	}
	a.saveDaemonSetting(key, nil)
}

func (a *app) previewSettingsCleanup() {
	a.syncSettingsSource()
	st := &a.settings.backend
	if st.general.operation.writing {
		return
	}
	source, generation := st.source, st.generation
	period := st.general.cleanupPeriod
	if period == "" {
		period = "Days90"
	}
	visible := make([]string, 0, len(a.sessions))
	localProjects := map[string]bool{}
	for _, project := range a.projects {
		if project.EnvironmentID == "" || project.EnvironmentID == settingsLocalEnvironment {
			localProjects[project.ID] = true
		}
	}
	for _, session := range a.sessions {
		if !session.Draft && !session.Archived && localProjects[session.ProjectID] {
			visible = append(visible, session.ID)
		}
	}
	binary := a.settingsStr("opencode_binary_path", "opencode")
	st.general.operation.writing, st.general.operation.err, st.general.cleanupPreview = true, "", nil
	st.general.cleanupTotal, st.general.cleanupDone = 0, 0
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
		defer cancel()
		var body struct {
			Preview settingsCleanupPreview `json:"preview"`
		}
		err := settingsRequest(ctx, source, map[string]any{"type": "previewSessionCleanup", "binary": binary, "period": period, "visibleSessionIds": visible}, "sessionCleanupPreview", &body)
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.general.operation.writing = false
			if err != nil {
				st.general.operation.err = tr("Cleanup preview failed. ") + err.Error()
				return
			}
			st.general.cleanupPreview = &body.Preview
		})
	}()
}

func (a *app) confirmSettingsCleanup() {
	st := &a.settings.backend
	if st.general.operation.writing || st.general.cleanupPreview == nil {
		return
	}
	preview := *st.general.cleanupPreview
	st.general.cleanupPreview, st.general.operation.writing, st.general.operation.err = nil, true, ""
	st.general.cleanupTotal, st.general.cleanupDone = len(preview.SessionIDs), 0
	source, generation := st.source, st.generation
	binary := a.settingsStr("opencode_binary_path", "opencode")
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 180*time.Second)
		defer cancel()
		deleted := []string{}
		var err error
		for _, id := range preview.SessionIDs {
			payload, requestErr := source.client.RequestFor(ctx, id, settingsLocalEnvironment, map[string]any{"type": "cleanupOldSession", "binary": binary, "before": preview.Before})
			if requestErr != nil {
				err = requestErr
				break
			}
			var body struct {
				Type    string          `json:"type"`
				Outcome json.RawMessage `json:"outcome"`
			}
			if json.Unmarshal(payload, &body) != nil || body.Type != "sessionCleanupFinished" {
				err = errors.New("Unexpected cleanup response; reload before retrying")
				break
			}
			if string(body.Outcome) == `"Skipped"` {
				continue
			}
			var outcome struct {
				Deleted *struct {
					IDs []string `json:"session_ids"`
				} `json:"Deleted"`
			}
			if json.Unmarshal(body.Outcome, &outcome) != nil || outcome.Deleted == nil {
				err = errors.New("Invalid cleanup outcome; reload before retrying")
				break
			}
			deleted = append(deleted, outcome.Deleted.IDs...)
			a.post(func() {
				if a.settingsSourceMatches(source, generation) {
					a.settings.backend.general.cleanupDone++
				}
			})
		}
		a.post(func() {
			if !a.settingsSourceMatches(source, generation) {
				return
			}
			st := &a.settings.backend
			st.general.operation.writing = false
			st.general.cleanupTotal, st.general.cleanupDone = 0, 0
			st.general.operation.message = fmt.Sprintf(tr("%d conversations removed"), len(deleted))
			if err != nil {
				st.general.operation.err = tr("Cleanup stopped; no request will be retried. ") + err.Error()
			}
			for _, id := range deleted {
				a.removeSessionLocal(id)
			}
		})
	}()
}

// settingsCleanupProgress is the running caption while the confirm loop
// removes sessions, mirroring Rust's Running{completed, total} phase.
func (a *app) settingsCleanupProgress() string {
	st := &a.settings.backend.general
	if st.operation.writing && st.cleanupTotal > 0 {
		return trf("Deleting %d of %d conversations", st.cleanupDone, st.cleanupTotal)
	}
	return ""
}

// settingsCleanupConfirmation is the compact apply prompt under the Session
// cleanup row: the eligible count and the delete controls, no per-session
// listing.
func (a *app) settingsCleanupConfirmation(c *ui.Context) {
	st := &a.settings.backend.general
	if st.cleanupPreview == nil {
		return
	}
	preview := st.cleanupPreview
	ui.Column(c).FillWidth().Margin(10, 20, 14, 20).Gap(8).Children(func() {
		ui.Text(c, fmt.Sprintf(tr("Permanently remove these %d eligible conversations and their history?"), len(preview.SessionIDs))).FontSize(12).TextColor(palette.text).Wrap()
		if progress := a.settingsCleanupProgress(); progress != "" {
			ui.Text(c, progress).FontSize(12).TextColor(palette.textSecondary)
		}
		tk := theme.TokensOf(c)
		ui.Row(c).Gap(8).Children(func() {
			cancel := action.Button(c, tr("Cancel"), action.ButtonProps{Variant: action.Text, Size: theme.Small})
			cancel.OnClick(func() { st.cleanupPreview = nil })
			del := action.Button(c, tr("Delete conversations"), action.ButtonProps{Variant: action.Primary, Size: theme.Small}).
				Disabled(len(preview.SessionIDs) == 0 || st.operation.writing)
			del.Background(tk.Error)
			if del.Hovered() {
				del.Background(tk.Error.Mix(tk.Text, 0.12))
			}
			del.TextColor(tk.BgContainer)
			del.OnClick(a.confirmSettingsCleanup)
		})
	})
}
