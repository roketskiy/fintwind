//go:build wiringe2e

package main

import (
	"context"
	"fmt"
	"time"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

func wiringSidebarHistory(ctx context.Context, r *wiringReport, root string) {
	a := wiringApp(ctx, nil, root)
	a.sidebarWidth = 300
	a.projects = append(a.projects,
		model.Project{ID: "empty", Name: "Empty project", CreatedAt: 1},
		model.Project{ID: "projectless", Name: "No project", CreatedAt: 1})
	a.branches = map[string]branchState{}
	for _, project := range a.projects {
		a.branches[project.ID] = branchState{root: project.Path, label: "fixture-main"}
	}
	for i := 0; i < 14; i++ {
		stamp := uint64(1000 - i)
		session := model.Session{ID: fmt.Sprintf("history-%02d", i), Title: fmt.Sprintf("History %02d", i),
			ProjectID: "fixture-project", Status: "idle", Draft: true, CreatedAt: stamp, LastReplyAt: &stamp,
			Messages: []model.Message{{Role: "user", Content: "Isolated sidebar fixture"}}}
		a.sessions = append(a.sessions, session)
	}
	a.sessions = append(a.sessions,
		model.Session{ID: "draft", ProjectID: "fixture-project", Draft: true},
		model.Session{ID: "archived", ProjectID: "fixture-project", Archived: true},
		model.Session{ID: "orphan-a", ProjectID: "missing-a", CreatedAt: 2},
		model.Session{ID: "orphan-b", ProjectID: "missing-b", CreatedAt: 3})
	a.selectedID = "history-00"
	a.detail = &a.sessions[1]
	a.rebuildRows()
	tt := ui.NewTester(a.sidebar, 320, 980)
	visibleSessions := func() int {
		n := 0
		for _, row := range a.rows {
			if row.Kind == rowSession {
				n++
			}
		}
		return n
	}
	r.check("sidebar initial history is capped", 6, visibleSessions())
	_, draftVisible := tt.AccessByRole(ui.RoleButton, "New task")
	r.check("sidebar new-task action is available", true, draftVisible)
	groups := []string{}
	for _, row := range a.rows {
		if row.Kind == rowGroupHeader {
			groups = append(groups, row.ID)
		}
	}
	r.check("sidebar preserves separate orphan projects and empty project", []string{"fixture-project", "missing-b", "missing-a", "empty"}, groups)
	r.check("sidebar search includes unrevealed and folded history", 17, len(a.paletteSessionEntries()))
	if !wiringClick(r, tt, "Show more") {
		return
	}
	r.check("sidebar first show-more reveals another six", 12, visibleSessions())
	if !wiringClick(r, tt, "Fixture project") {
		return
	}
	r.check("sidebar header collapses without changing selection", []any{0, "history-00"}, []any{visibleSessions(), a.selectedID})
	tt.Key(0, ui.KeyRight)
	r.check("sidebar right arrow unfolds the focused project", 12, visibleSessions())
	tt.Key(0, ui.KeyLeft)
	r.check("sidebar left arrow folds the focused project", 0, visibleSessions())
	for range 3 {
		tt.Frame()
	}
	a.revealSidebarSession("history-13")
	tt.Frame()
	r.check("sidebar reveal expands history through the older target", 15, visibleSessions())
	_, targetVisible := tt.AccessByRole(ui.RoleButton, "History 13")
	r.check("sidebar reveal scrolls the target into the viewport", true, targetVisible)
	if !wiringClick(r, tt, "History 13") {
		return
	}
	r.check("sidebar card activation selects the session", "history-13", a.selectedID)
	// Open the production context menu by keyboard, then edit through its input.
	tt.Key(ui.Shift, ui.KeyF10)
	if !wiringClick(r, tt, "Rename") {
		return
	}
	tt.Type("Renamed fixture")
	input, _ := tt.AccessByRole(ui.RoleTextField, "Rename session")
	r.check("sidebar rename initially selects the entire title", "Renamed fixture", input.Value)
	tt.Key(0, ui.KeyEscape)
	for range 3 {
		tt.Frame()
	}
	r.check("sidebar Escape cancels inline rename", "History 13", a.summarySession("history-13").DisplayTitle())
	a.beginSessionRename("history-13")
	tt.Frame()
	wiringType(r, tt, "Rename session", "Renamed fixture")
	tt.Key(0, ui.KeyEnter)
	for range 3 {
		tt.Frame()
	}
	r.check("sidebar Enter commits inline rename", "Renamed fixture", a.summarySession("history-13").DisplayTitle())
	r.check("sidebar rename does not promote conversation recency", "history-00", a.sortedGroupSessions("fixture-project")[0].ID)
	a.beginSessionRename("history-13")
	tt.Frame()
	wiringType(r, tt, "Rename session", "Outside commit")
	wiringClick(r, tt, "Search")
	r.check("sidebar outside click commits inline rename", "Outside commit", a.summarySession("history-13").DisplayTitle())
	r.check("sidebar search opens the command palette", true, a.paletteOpen)
	a.paletteOpen = false
	a.setSidebarCollapsed("fixture-project", true)
	for range 3 {
		tt.Frame()
	}
	a.revealSidebarSession("history-00")
	for range 3 {
		tt.Frame()
	}
	r.check("sidebar row identity survives fold and rename", "Outside commit", a.summarySession("history-13").DisplayTitle())
	working := a.summarySession("history-00")
	working.Status = "working"
	working.Turns = []model.AgentTurn{{Status: "running", StartedAt: uint64(time.Now().Unix() - 65)}}
	tt.Frame()
	r.check("sidebar running turn shows elapsed time", true, tt.HasText("Working for 1m 5s"))
	working.Status = "idle"
	for range 3 {
		tt.Frame()
	}
	working.Workspace = model.SessionWorkspace{Kind: "worktree", Branch: "fixture-worktree"}
	tt.Frame()
	r.check("sidebar uses the session worktree branch", true, tt.HasText("fixture-worktree"))
	a.setSidebarCollapsed("fixture-project", true)
	tt.Frame()
	if !wiringClick(r, tt, "Empty project") {
		return
	}
	tt.Key(ui.Shift, ui.KeyF10)
	if !wiringClick(r, tt, "Remove from app") {
		return
	}
	r.check("sidebar project menu opens removal confirmation", "empty", a.projectRemoveConfirm)
	wiringClick(r, tt, "Cancel")
	r.check("sidebar project removal cancellation retains project", true, a.projectForID("empty") != nil && a.projectRemoveConfirm == "")
}
