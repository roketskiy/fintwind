//go:build wiringe2e

package main

import (
	"context"
	"time"

	"github.com/egoist/mygo/ui"
)

// wiringSkillsCatalog is the skill library the loopback fixture serves: one
// grouped skill installed for two ecosystems, one disabled user skill, and
// one project skill. It mirrors the daemon's camelCase SkillEntry shape,
// including the fields the detail pane reads.
func wiringSkillsCatalog(modifiedAt uint64) []any {
	tools := "Bash, Read"
	return []any{
		map[string]any{
			"name": "demo-deploy", "description": "Deploy runbook",
			"scope": "user", "project": "",
			"installs": []any{
				map[string]any{"source": "shared", "dir": `C:\skills\demo-deploy`, "skillFile": `C:\skills\demo-deploy\SKILL.md`, "enabled": true},
				map[string]any{"source": "opencode", "dir": `C:\opencode\skills\demo-deploy`, "skillFile": `C:\opencode\skills\demo-deploy\SKILL.md`, "enabled": true},
			},
			"enabled": true, "allowedTools": tools,
			"body": "# Demo\n\nRunbook body.", "supportingFiles": 2,
			"totalBytes": uint64(3300), "modifiedAt": modifiedAt, "duplicates": 2, "rowKey": uint64(7),
		},
		map[string]any{
			"name": "quiet-review", "description": "",
			"scope": "user", "project": "",
			"installs": []any{
				map[string]any{"source": "shared", "dir": `C:\skills\quiet-review`, "skillFile": `C:\skills\quiet-review\SKILL.md`, "enabled": false},
			},
			"enabled": false, "allowedTools": nil,
			"body": "", "supportingFiles": 0,
			"totalBytes": uint64(412), "modifiedAt": nil, "duplicates": 0, "rowKey": uint64(9),
		},
		map[string]any{
			"name": "fixture-task", "description": "Project helper",
			"scope": "project", "project": "Fixture project",
			"installs": []any{
				map[string]any{"source": "shared", "dir": `C:\repo\.skills\fixture-task`, "skillFile": `C:\repo\.skills\fixture-task\SKILL.md`, "enabled": true},
			},
			"enabled": true, "allowedTools": nil,
			"body": "Task notes.", "supportingFiles": 1,
			"totalBytes": uint64(1200), "modifiedAt": nil, "duplicates": 0, "rowKey": uint64(11),
		},
	}
}

// wiringSkills drives the reworked Skills settings page headless: grouped
// roster, source/search filters, footer captions, detail pane, and the
// toggle/trash wires. It never opens a window.
func wiringSkills(ctx context.Context, r *wiringReport, a *app, f *wiringFixture, root string) {
	a.settings.backend.source = settingsSource{client: a.client, identity: a.client.Identity(), directory: root}
	catalog := wiringSkillsCatalog(uint64(time.Now().Unix()) - 300)
	f.extended = func(e wiringEnvelope) (any, error, bool, bool) {
		switch e.Command.Type {
		case "loadSkills":
			return map[string]any{"type": "skillsCatalog", "catalog": map[string]any{"skills": catalog}}, nil, false, true
		case "setSkillsEnabled", "trashSkills":
			return map[string]string{"type": "ack"}, nil, false, true
		}
		return nil, nil, false, false
	}
	defer func() { f.extended = nil }()
	queue, release := installHeadlessDispatcher(a)
	defer release()

	a.loadSettingsSkills()
	r.check("skills catalog loads through the daemon wire", true, wiringApplyUntil(ctx, queue, func() bool {
		st := &a.settings.backend
		return st.skill.loaded && !st.skill.loading
	}))

	view := func(c *ui.Context) {
		if !a.mdReady {
			a.mdCompact = newMarkdownOptions(c, 11.5, 17, 10.5, 16)
			a.mdReady = true
		}
		a.skillsSettings(c)
	}
	tt := ui.NewTester(view, 1000, 700)
	r.check("skills roster groups user skills with counts", true, tt.HasText("USER SKILLS 2"))
	r.check("skills roster groups the project scope", true, tt.HasText("FIXTURE PROJECT 1"))
	r.check("skills roster shows the grouped skill", true, tt.HasText("demo-deploy"))
	r.check("skills roster falls back to sources for a missing description", true, tt.HasText("Shared · OpenCode"))
	r.check("skills roster marks the disabled skill in text", true, tt.HasText("Disabled"))
	r.check("skills roster footer counts skills and disabled", true, tt.HasText("3 skills · 1 disabled"))
	r.check("skills detail selects the first skill", true, tt.HasText("Deploy runbook"))
	r.check("skills detail captions scope", true, tt.HasText("available in every project"))
	r.check("skills detail shows the invoke row", true, tt.HasText("Invoke") && tt.HasText("/demo-deploy"))
	r.check("skills detail shows locations per copy", true, tt.HasText("Shared") && tt.HasText("OpenCode"))
	r.check("skills detail shows contents and updated rows", true, tt.HasText("Contents") && tt.HasText("Updated") && tt.HasText("5m ago"))
	r.check("skills detail shows allowed tools", true, tt.HasText("Tools"))
	r.check("skills detail warns about duplicates", true, tt.HasText("most specific copy wins"))
	r.check("skills detail offers open, reveal, copy and delete", true, tt.HasText("Open SKILL.md") && tt.HasText("Show in File Manager") && tt.HasText("Copy Path") && tt.HasText("Delete"))
	r.check("skills detail renders the skill document", true, tt.HasText("Runbook body."))
	r.check("skills detail labels the enable switch", true, func() bool {
		_, found := tt.AccessByRole(ui.RoleSwitch, "Disable demo-deploy")
		return found
	}())

	// Search narrows the roster and the footer reports the filter.
	a.settings.catalog.skillQuery = "fixture"
	tt.Frame()
	r.check("skills search narrows to the project skill", true, tt.HasText("fixture-task") && !tt.HasText("demo-deploy"))
	r.check("skills search footer reports shown of total", true, tt.HasText("1 of 3 shown"))
	r.check("skills search moves the detail to the visible skill", true, tt.HasText("Project helper"))

	// The source filter keeps a grouped skill when any copy matches.
	a.settings.catalog.skillQuery = ""
	a.settings.catalog.skillSource = "opencode"
	tt.Frame()
	r.check("skills source filter keeps grouped installs", true, tt.HasText("demo-deploy") && !tt.HasText("fixture-task"))
	r.check("skills source filter footer reports shown of total", true, tt.HasText("1 of 3 shown"))

	// A row click selects the skill and its detail follows.
	a.settings.catalog.skillSource = ""
	tt.Frame()
	wiringClick(r, tt, "fixture-task")
	tt.Frame()
	r.check("skills row click selects the skill", `C:\repo\.skills\fixture-task`, a.settings.backend.skillSelected)
	r.check("skills detail follows the selection", true, tt.HasText("in Fixture project"))
	r.check("skills detail labels a single location", true, tt.HasText("Location"))

	// The switch answers immediately, then the rescan confirms from disk.
	wiringRoleClick(r, tt, ui.RoleSwitch, "Disable fixture-task")
	tt.Frame()
	enabled := true
	for _, skill := range a.settings.backend.skills {
		if skill.key() == `C:\repo\.skills\fixture-task` {
			enabled = skill.Enabled
		}
	}
	r.check("skills toggle answers immediately", false, enabled)
	r.check("skills toggle rescan lands", true, wiringApplyUntil(ctx, queue, func() bool {
		st := &a.settings.backend
		return !st.skill.writing && !st.skill.loading && st.skill.loaded
	}))
	r.check("skills toggle reaches the daemon", 1, f.count("setSkillsEnabled"))

	// Delete arms, then moves every copy to the Trash.
	wiringClick(r, tt, "Delete")
	tt.Frame()
	r.check("skills delete arms before trashing", true, tt.HasText("Move to Trash"))
	wiringClick(r, tt, "Move to Trash")
	tt.Frame()
	r.check("skills trash removes the row at once", true, !tt.HasText("fixture-task"))
	r.check("skills trash rescan lands", true, wiringApplyUntil(ctx, queue, func() bool {
		st := &a.settings.backend
		return !st.skill.writing && !st.skill.loading && st.skill.loaded
	}))
	r.check("skills trash reaches the daemon", 1, f.count("trashSkills"))

	// Honest unloaded and empty states, the way the GPUI page reads.
	savedSkills, savedOp, savedSelected := a.settings.backend.skills, a.settings.backend.skill, a.settings.backend.skillSelected
	a.settings.backend.skills, a.settings.backend.skillSelected = nil, ""
	a.settings.backend.skill.loaded, a.settings.backend.skill.loading = false, false
	tt.Frame()
	r.check("skills scanning state reads honestly", true, tt.HasText("Scanning skill folders…"))
	a.settings.backend.skills, a.settings.backend.skill.loaded = []settingsSkill{}, true
	tt.Frame()
	r.check("skills empty state explains the library", true, tt.HasText("No skills yet") && tt.HasText("SKILL.md"))
	a.settings.backend.skills, a.settings.backend.skill, a.settings.backend.skillSelected = savedSkills, savedOp, savedSelected
}
