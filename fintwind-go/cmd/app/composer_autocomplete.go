package main

import (
	"context"
	"encoding/json"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/egoist/mygo/ui"
)

// composer_autocomplete.go owns the composer's / and @ popup: trigger
// parsing over the draft plus caret, prefetched slash/file indexes, fuzzy
// filtering, keyboard accept, and click accept.
//
// Rust parity: src/app/autocomplete.rs (popup over prefetched indexes,
// first row selected, up/down/enter/tab/esc) + composer_complete.rs
// (detect_trigger rules, FILTER_CAP=64, @ lists skills then files).
//
// mygo constraint: TextKey exposes only Text()/SetText(), never the caret.
// The view caches TextSelection() (rune offsets) every frame; OnTextKey
// reuses that cache because navigation keys never move the caret before
// the handler runs. Insertion via SetText drops the caret to the end, so
// the accepted caret is stored and applied next frame with
// SetTextSelection.

// autocompleteTrigger is one / or @ site under the caret. Offsets are in
// runes into the draft, matching ui TextSelection units.
type autocompleteTrigger struct {
	command bool
	query   string
	start   int
	end     int
}

// autocompleteRow is one filtered popup row. It mirrors AutocompleteRow in
// src/app/autocomplete.rs: commands show sigil+name, hint, description and
// scope; files show basename plus parent dir. Insertion always uses the full
// name/path, never the drawn split.
type autocompleteRow struct {
	command bool
	skill   bool
	name    string
	title   string
	hint    string
	detail  string
	scope   string
	isDir   bool
}

// slashCommand is the popup's command view: discovered plus reported plus
// builtins. Template presence decides nothing here; the popup inserts the
// name and the daemon expands it at submit, mirroring the Rust send path.
type slashCommand struct {
	name        string
	description string
	scope       string
	hint        string
	skill       bool
	template    *string
}

// mentionFile is one ListProjectFiles entry.
type mentionFile struct {
	path  string
	isDir bool
}

const (
	autocompleteCap      = 64
	autocompleteSkillCap = 32
	// Popup shell mirrors render_composer_autocomplete: max 302 high,
	// 11 radius, strong border on raised. Rows are 30 high, 7 radius.
	autocompleteListH = 302.0
	autocompleteRowH  = 30.0
)

// builtinSlashCommands mirrors builtin_fintwind_commands names so the popup
// lists something before discovery returns.
func builtinSlashCommands() []slashCommand {
	return []slashCommand{
		{name: "init", description: "Analyze this repo and write AGENTS.md", scope: "builtin"},
		{name: "review", description: "Review pending changes, ordered by severity", scope: "builtin"},
		{name: "commit", description: "Stage and commit with a clear message", scope: "builtin"},
		{name: "compact", description: "Free up context", scope: "builtin"},
		{name: "reload-server", description: "Hot-reload the native OpenCode server", scope: "builtin"},
	}
}

// detectAutocompleteTrigger mirrors composer_complete::detect_trigger in rune
// offsets (mygo selections count runes). A slash command must be the first
// token of its line: any whitespace in the query ends the trigger. An @
// mention starts at a token boundary; "user@host" never triggers because the
// token does not open with @.
func detectAutocompleteTrigger(draft string, caret int) *autocompleteTrigger {
	runes := []rune(draft)
	if caret < 0 {
		caret = 0
	}
	if caret > len(runes) {
		caret = len(runes)
	}
	if caret == 0 {
		return nil
	}
	// Line start before caret.
	lineStart := 0
	for i := caret - 1; i >= 0; i-- {
		if runes[i] == '\n' {
			lineStart = i + 1
			break
		}
	}
	if line := string(runes[lineStart:caret]); strings.HasPrefix(line, "/") {
		query := line[1:]
		for _, r := range query {
			if r == ' ' || r == '\t' || r == '\n' || r == '\r' {
				return nil
			}
		}
		return &autocompleteTrigger{command: true, query: query, start: lineStart, end: caret}
	}
	// Token start before caret.
	tokenStart := 0
	for i := caret - 1; i >= 0; i-- {
		if runes[i] == ' ' || runes[i] == '\t' || runes[i] == '\n' || runes[i] == '\r' {
			tokenStart = i + 1
			break
		}
	}
	token := string(runes[tokenStart:caret])
	if !strings.HasPrefix(token, "@") {
		return nil
	}
	return &autocompleteTrigger{query: token[1:], start: tokenStart, end: caret}
}

// fuzzyScore ranks target against query: prefix > substring > subsequence.
// -1 means no match. Case-insensitive throughout.
func fuzzyScore(target, query string) int {
	if query == "" {
		return 0
	}
	lowTarget := strings.ToLower(target)
	lowQuery := strings.ToLower(query)
	if strings.HasPrefix(lowTarget, lowQuery) {
		return 300 + len(lowQuery)*10 - len(lowTarget)
	}
	if strings.Contains(lowTarget, lowQuery) {
		return 200 + len(lowQuery)*10 - len(lowTarget)
	}
	// Subsequence fallback so "ap" still finds "src/app.rs".
	ti := 0
	for _, qr := range lowQuery {
		found := false
		for ti < len(lowTarget) {
			r, size := utf8.DecodeRuneInString(lowTarget[ti:])
			ti += size
			if r == qr {
				found = true
				break
			}
		}
		if !found {
			return -1
		}
	}
	return 100 - len(lowTarget)
}

// filterAutocomplete caps at 64, best first, stable for equal scores.
func filterAutocomplete(targets []string, query string) []int {
	type scored struct {
		index int
		score int
	}
	if strings.TrimSpace(query) == "" {
		out := make([]int, 0, minInt(autocompleteCap, len(targets)))
		for i := range targets {
			if len(out) >= autocompleteCap {
				break
			}
			out = append(out, i)
		}
		return out
	}
	scoredList := make([]scored, 0, len(targets))
	for i, target := range targets {
		if score := fuzzyScore(target, query); score >= 0 {
			scoredList = append(scoredList, scored{index: i, score: score})
		}
	}
	// Insertion-ordered bubble is O(n^2); n is small after discovery caps.
	for i := 0; i < len(scoredList); i++ {
		for j := i + 1; j < len(scoredList); j++ {
			if scoredList[j].score > scoredList[i].score {
				scoredList[i], scoredList[j] = scoredList[j], scoredList[i]
			}
		}
	}
	out := make([]int, 0, autocompleteCap)
	for _, s := range scoredList {
		if len(out) >= autocompleteCap {
			break
		}
		out = append(out, s.index)
	}
	return out
}

func minInt(a, b int) int {
	if a < b {
		return a
	}
	return b
}

// autocompleteSourcesKey identifies the cached index: project path plus the
// reported command revision. Workspace switches invalidate it.
func (a *app) autocompleteSourcesKey() string {
	project := a.selectedProject()
	root := ""
	if project != nil {
		root = project.Path
	}
	reported := 0
	if a.detail != nil {
		if ext, ok := a.live[a.detail.ID]; ok && ext != nil && ext.extension != nil {
			if cmds, ok := ext.extension["availableCommands"].([]any); ok {
				reported = len(cmds)
			}
		}
	}
	return root + "\x00" + string(rune(reported))
}

// refreshAutocompleteSources prefetches slash commands and project files off
// the UI thread. Render reads only the cached slices, never the filesystem.
func (a *app) refreshAutocompleteSources() {
	if a.client == nil {
		return
	}
	project := a.selectedProject()
	if project == nil || project.Path == "" || !sessionProjectIsLocal(project) {
		if a.slashIndex == nil {
			a.slashIndex = builtinSlashCommands()
		}
		if a.mentionFiles == nil {
			a.mentionFiles = []mentionFile{}
		}
		return
	}
	key := project.Path
	if a.autocompleteDir == key && (a.slashIndex != nil || a.autocompleteLoading) {
		if a.mentionFiles != nil || a.autocompleteLoading {
			return
		}
	}
	if a.autocompleteLoading {
		return
	}
	a.autocompleteLoading = true
	a.autocompleteDir = key
	if a.slashIndex == nil {
		a.slashIndex = builtinSlashCommands()
	}
	client := a.client
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		commands := builtinSlashCommands()
		if payload, err := client.Request(ctx, map[string]any{"type": "workspace", "operation": map[string]any{
			"type": "discoverSlashCommands", "project_root": key, "binary": nil,
		}}); err == nil {
			var envelope struct {
				Type   string          `json:"type"`
				Result json.RawMessage `json:"result"`
			}
			if json.Unmarshal(payload, &envelope) == nil && envelope.Type == "workspace" {
				var result struct {
					Commands []struct {
						Name         string  `json:"name"`
						Description  string  `json:"description"`
						Scope        string  `json:"scope"`
						ArgumentHint *string `json:"argument_hint"`
						Template     *string `json:"template"`
					} `json:"commands"`
				}
				if json.Unmarshal(envelope.Result, &result) == nil && result.Commands != nil {
					seen := map[string]bool{}
					commands = nil
					for _, cmd := range result.Commands {
						if cmd.Name == "" || seen[cmd.Name] {
							continue
						}
						seen[cmd.Name] = true
						hint := ""
						if cmd.ArgumentHint != nil {
							hint = *cmd.ArgumentHint
						}
						commands = append(commands, slashCommand{
							name: cmd.Name, description: cmd.Description,
							scope: cmd.Scope, hint: hint,
							skill:    cmd.Scope == "skill" || cmd.Scope == "Skill",
							template: cmd.Template,
						})
					}
				}
			}
		}
		files := []mentionFile{}
		if payload, err := client.Request(ctx, map[string]any{"type": "workspace", "operation": map[string]any{
			"type": "listProjectFiles", "root": key, "cap": 50000,
		}}); err == nil {
			var envelope struct {
				Type   string          `json:"type"`
				Result json.RawMessage `json:"result"`
			}
			if json.Unmarshal(payload, &envelope) == nil && envelope.Type == "workspace" {
				var result struct {
					Entries []struct {
						Path  string `json:"path"`
						IsDir bool   `json:"is_dir"`
					} `json:"entries"`
				}
				if json.Unmarshal(envelope.Result, &result) == nil {
					for _, entry := range result.Entries {
						if entry.Path == "" {
							continue
						}
						files = append(files, mentionFile{path: entry.Path, isDir: entry.IsDir})
						if len(files) >= 50000 {
							break
						}
					}
				}
			}
		}
		dir := key
		a.post(func() {
			if a.autocompleteDir != dir {
				return
			}
			a.autocompleteLoading = false
			a.slashIndex = mergeReportedCommands(commands, a.reportedSlashCommands())
			a.mentionFiles = files
			a.autocompleteGen++
			a.autocompleteMemoKey = ""
			a.autocompleteMemoRows = nil
		})
	}()
}

// reportedSlashCommands folds the daemon's live availableCommands event into
// the discovered list, filling gaps without displacing metadata.
func (a *app) reportedSlashCommands() []slashCommand {
	if a.detail == nil {
		return nil
	}
	state := a.live[a.detail.ID]
	if state == nil || state.extension == nil {
		return nil
	}
	raw, _ := state.extension["availableCommands"].([]any)
	out := make([]slashCommand, 0, len(raw))
	for _, item := range raw {
		switch v := item.(type) {
		case map[string]any:
			name, _ := v["name"].(string)
			if name == "" {
				continue
			}
			desc, _ := v["description"].(string)
			out = append(out, slashCommand{name: name, description: desc, scope: "builtin"})
		case string:
			if v != "" {
				out = append(out, slashCommand{name: v, scope: "builtin"})
			}
		}
	}
	return out
}

func mergeReportedCommands(discovered, reported []slashCommand) []slashCommand {
	if len(reported) == 0 {
		return discovered
	}
	seen := map[string]bool{}
	for _, cmd := range discovered {
		seen[cmd.name] = true
	}
	out := append([]slashCommand(nil), discovered...)
	for _, cmd := range reported {
		if cmd.name == "" || seen[cmd.name] {
			continue
		}
		seen[cmd.name] = true
		out = append(out, cmd)
	}
	return out
}

// autocompleteRows filters the cached indexes for trigger. Results are
// memoized on (kind, query, index generation) so a caret blink re-renders
// without re-fuzzy-matching the workspace, mirroring ResultsMemo.
func (a *app) autocompleteRows(trigger *autocompleteTrigger) []autocompleteRow {
	if trigger == nil {
		return nil
	}
	key := trigger.query + "\x00" + a.autocompleteIndexKey(trigger.command)
	if key == a.autocompleteMemoKey && a.autocompleteMemoRows != nil {
		rows := a.autocompleteMemoRows
		if a.autocompleteHighlight >= len(rows) {
			a.autocompleteHighlight = 0
		}
		if len(rows) == 0 {
			return nil
		}
		return rows
	}
	rows := a.filterAutocompleteRows(trigger)
	a.autocompleteMemoKey = key
	a.autocompleteMemoRows = rows
	if a.autocompleteHighlight >= len(rows) {
		a.autocompleteHighlight = 0
	}
	if len(rows) == 0 {
		return nil
	}
	return rows
}

// autocompleteIndexKey identifies the filtered sources: the kind plus the
// generation that refreshAutocompleteSources bumps on every refill.
func (a *app) autocompleteIndexKey(command bool) string {
	kind := "@"
	if command {
		kind = "/"
	}
	return kind + "\x00" + string(rune(a.autocompleteGen)) + "\x00" +
		string(rune(len(a.slashIndex))) + "\x00" + string(rune(len(a.mentionFiles)))
}

// filterAutocompleteRows is the uncached filter pass: commands for /,
// skills then files for @. Empty query lists the head in index order.
func (a *app) filterAutocompleteRows(trigger *autocompleteTrigger) []autocompleteRow {
	if trigger == nil {
		return nil
	}
	if trigger.command {
		targets := make([]string, 0, len(a.slashIndex))
		for _, cmd := range a.slashIndex {
			targets = append(targets, cmd.name)
		}
		rows := make([]autocompleteRow, 0, autocompleteCap)
		for _, index := range filterAutocomplete(targets, trigger.query) {
			cmd := a.slashIndex[index]
			sigil := "/"
			if cmd.skill {
				sigil = "@"
			}
			rows = append(rows, autocompleteRow{
				command: true, skill: cmd.skill, name: cmd.name,
				title: sigil + cmd.name, hint: cmd.hint,
				detail: cmd.description, scope: scopeLabel(cmd.scope),
			})
		}
		return rows
	}
	rows := make([]autocompleteRow, 0, autocompleteCap)
	// Skills first, up to half the cap, mirroring Rust's @ rows.
	skills := make([]string, 0)
	skillIdx := make([]int, 0)
	for i, cmd := range a.slashIndex {
		if cmd.skill {
			skills = append(skills, cmd.name)
			skillIdx = append(skillIdx, i)
		}
	}
	matched := 0
	for _, si := range filterAutocomplete(skills, trigger.query) {
		if matched >= autocompleteSkillCap {
			break
		}
		cmd := a.slashIndex[skillIdx[si]]
		rows = append(rows, autocompleteRow{
			skill: true, name: cmd.name,
			title: "@" + cmd.name, hint: cmd.hint,
			detail: cmd.description, scope: scopeLabel(cmd.scope),
		})
		matched++
	}
	remaining := autocompleteCap - len(rows)
	if remaining > 0 {
		targets := make([]string, 0, len(a.mentionFiles))
		for _, f := range a.mentionFiles {
			targets = append(targets, f.path)
		}
		for _, fi := range filterAutocomplete(targets, trigger.query) {
			if len(rows) >= autocompleteCap {
				break
			}
			f := a.mentionFiles[fi]
			// Rust draws basename prominently with the parent dir as the
			// secondary line; insertion still uses the full path.
			trimmed := strings.TrimSuffix(f.path, "/")
			base := trimmed
			parent := ""
			if i := strings.LastIndex(trimmed, "/"); i >= 0 {
				base = trimmed[i+1:]
				parent = trimmed[:i]
			}
			if f.isDir {
				base += "/"
			}
			rows = append(rows, autocompleteRow{
				name: f.path, title: base, detail: parent,
				isDir: f.isDir,
			})
		}
	}
	return rows
}

// scopeLabel normalizes a discovered scope for the row badge, mirroring
// CommandScope::label without localized strings.
func scopeLabel(scope string) string {
	switch strings.ToLower(scope) {
	case "project":
		return "project"
	case "user":
		return "user"
	case "skill":
		return "skill"
	default:
		return "builtin"
	}
}

// currentAutocompleteTrigger derives the popup trigger from the cached
// render-time draft and caret. It also resets highlight/dismiss on change.
func (a *app) currentAutocompleteTrigger() (*autocompleteTrigger, []autocompleteRow) {
	trigger := detectAutocompleteTrigger(a.draft, a.composerCaretEnd)
	key := ""
	if trigger != nil {
		key = string(rune(len(trigger.query))) + trigger.query + string(rune(trigger.start)) + string(rune(trigger.end))
		if trigger.command {
			key = "/" + key
		} else {
			key = "@" + key
		}
	}
	if key != a.autocompleteKey {
		a.autocompleteKey = key
		a.autocompleteHighlight = 0
		a.autocompleteDismissed = false
	}
	if trigger == nil || a.autocompleteDismissed {
		return nil, nil
	}
	rows := a.autocompleteRows(trigger)
	if len(rows) == 0 {
		return nil, nil
	}
	if a.autocompleteHighlight >= len(rows) {
		a.autocompleteHighlight = 0
	}
	return trigger, rows
}

// applyAutocompleteInsert replaces the trigger range with insert text and
// records the caret for the next frame. Runs inside OnTextKey via SetText.
func applyAutocompleteInsert(draft string, trigger *autocompleteTrigger, insert string) (string, int) {
	runes := []rune(draft)
	if trigger.start < 0 {
		trigger.start = 0
	}
	if trigger.end > len(runes) {
		trigger.end = len(runes)
	}
	if trigger.start > trigger.end {
		trigger.start = trigger.end
	}
	next := string(runes[:trigger.start]) + insert + string(runes[trigger.end:])
	caret := trigger.start + len([]rune(insert))
	return next, caret
}

// renderAutocompleteList builds the popup body: rows only, no shell. The
// caller mounts it in a PopoverBase anchored to the composer card so it
// floats above the card like render_composer_autocomplete instead of pushing
// the layout. Row geometry mirrors the Rust rows: 30 high, 7 radius, 8 gap.
func (a *app) renderAutocompleteList(c *ui.Context, trigger *autocompleteTrigger, rows []autocompleteRow) {
	ui.Scroll(c).FillWidth().MaxHeight(autocompleteListH).Children(func() {
		ui.Column(c).FillWidth().Padding(4).Gap(2).Children(func() {
			for index, row := range rows {
				index := index
				row := row
				highlighted := index == a.autocompleteHighlight
				item := ui.ButtonBase(c).Label(row.title).FillWidth().
					Height(autocompleteRowH).Padding(0, 8).Radius(7).
					Gap(8).Justify(ui.Start).AlignItems(ui.Center)
				if highlighted {
					item.Background(palette.overlayStrong)
				} else if item.Hovered() {
					item.Background(palette.overlay)
				}
				item.Children(func() {
					iconName := "command"
					if row.skill {
						iconName = "package"
					}
					if trigger.command || row.skill {
						icon(c, iconName, 12, palette.textTertiary)
					} else if !row.isDir {
						brandIcon(c, fileTypeIcon(row.name), 13)
					}
					if trigger.command || row.skill {
						ui.Row(c).MaxWidth(260).Children(func() {
							ui.Text(c, row.title).FontSize(uiPx(12)).FontWeight(500).
								TextColor(palette.text).SingleLine().Ellipsis("...")
						})
						if row.hint != "" {
							ui.Text(c, row.hint).FontSize(uiPx(11)).
								TextColor(palette.textMuted).SingleLine().Ellipsis("...")
						}
						ui.Row(c).Grow(1).MinWidth(0).Children(func() {
							ui.Text(c, row.detail).FontSize(uiPx(11)).
								TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
						})
						if row.scope != "" {
							ui.Row(c).Height(16).Padding(0, 5).Radius(4).
								Border(1, palette.border).AlignItems(ui.Center).Children(func() {
								ui.Text(c, row.scope).FontSize(uiPx(9)).FontWeight(600).
									TextColor(palette.textTertiary)
							})
						}
					} else {
						ui.Row(c).MaxWidth(300).Children(func() {
							ui.Text(c, row.title).FontSize(uiPx(12)).
								TextColor(palette.text).SingleLine().Ellipsis("...")
						})
						if row.detail != "" {
							ui.Row(c).Grow(1).MinWidth(0).Children(func() {
								ui.Text(c, row.detail).FontSize(uiPx(11)).
									TextColor(palette.textGhost).SingleLine().Ellipsis("...")
							})
						}
					}
				})
				if item.Clicked() {
					a.acceptAutocompleteRow(trigger, row)
				}
			}
		})
	})
}

// acceptAutocompleteRow inserts the clicked row outside a TextKey context by
// updating the draft and scheduling the caret for the next frame.
func (a *app) acceptAutocompleteRow(trigger *autocompleteTrigger, row autocompleteRow) {
	if trigger == nil {
		return
	}
	insert := ""
	if trigger.command {
		insert = "/" + row.name + " "
	} else if row.skill {
		insert = "@" + row.name + " "
	} else {
		insert = "@" + row.name + " "
	}
	next, caret := applyAutocompleteInsert(a.draft, trigger, insert)
	a.draft = next
	a.pendingCaret = &caret
	a.autocompleteDismissed = true
	a.autocompleteKey = ""
	if a.detail != nil {
		state := a.stateForSession(a.detail)
		state.draft = next
	}
}
