package main

import (
	"path/filepath"
	"sort"
	"strings"

	"fintwind-go/internal/model"
)

type sidebarRowKind uint8

const (
	rowGroupHeader sidebarRowKind = iota
	rowSession
	rowShowMore
	rowSpacer
)

type sidebarRow struct {
	Kind      sidebarRowKind
	ID        string
	Project   *model.Project
	Remaining int
}

func (r sidebarRow) key() any {
	return struct {
		Kind sidebarRowKind
		ID   string
	}{r.Kind, r.ID}
}

type sidebarGroup struct {
	project  *model.Project
	sessions []*model.Session
	recency  uint64
}

func (a *app) sidebarProjectless(project *model.Project) bool {
	if project.Name == "No project" {
		return true
	}
	if a.projectlessRoot == "" || project.Path == "" {
		return false
	}
	relative, err := filepath.Rel(a.projectlessRoot, project.Path)
	return err == nil && relative != ".." && !strings.HasPrefix(relative, ".."+string(filepath.Separator))
}

// Rebuild only at catalog and activity changes. Drawing visits the virtual
// list's visible rows and reads branch results already published by workers.
func (a *app) rebuildRows() {
	if a.applyingEvents {
		a.sidebarDirty = true
		return
	}
	projects := make(map[string]*model.Project, len(a.projects))
	for i := range a.projects {
		projects[a.projects[i].ID] = &a.projects[i]
	}
	sessions := make([]*model.Session, 0, len(a.sessions))
	a.byID = make(map[string]*model.Session, len(a.sessions))
	seen := make(map[string]bool, len(a.sessions))
	for i := range a.sessions {
		session := &a.sessions[i]
		a.byID[session.ID] = session
		if session.Archived || !session.HasStarted() || seen[session.ID] {
			continue
		}
		seen[session.ID] = true
		sessions = append(sessions, session)
	}
	sort.SliceStable(sessions, func(i, j int) bool {
		return sessionStamp(sessions[i]) > sessionStamp(sessions[j])
	})
	groups := make(map[string]*sidebarGroup, len(projects))
	order := make([]*sidebarGroup, 0, len(projects))
	for _, session := range sessions {
		group := groups[session.ProjectID]
		if group == nil {
			project := projects[session.ProjectID]
			if project == nil {
				project = &model.Project{ID: session.ProjectID, Name: "Unknown project"}
			}
			group = &sidebarGroup{project: project, recency: sessionStamp(session)}
			groups[session.ProjectID] = group
			order = append(order, group)
		}
		group.sessions = append(group.sessions, session)
	}
	for i := range a.projects {
		project := &a.projects[i]
		if groups[project.ID] == nil && !a.sidebarProjectless(project) {
			group := &sidebarGroup{project: project, recency: project.CreatedAt}
			groups[project.ID] = group
			order = append(order, group)
		}
	}
	sort.SliceStable(order, func(i, j int) bool { return order[i].recency > order[j].recency })
	if a.collapsed == nil {
		a.collapsed = make(map[string]bool)
	}
	rows := make([]sidebarRow, 0, len(sessions)+len(order)*2)
	selected := a.summarySession(a.selectedID)
	for _, group := range order {
		id := group.project.ID
		if _, known := a.collapsed[id]; !known {
			a.collapsed[id] = selected == nil || selected.ProjectID != id
		}
		rows = append(rows, sidebarRow{Kind: rowGroupHeader, ID: id, Project: group.project})
		if !a.collapsed[id] {
			visible := min(a.sidebarGroupVisible(id), len(group.sessions))
			for _, session := range group.sessions[:visible] {
				rows = append(rows, sidebarRow{Kind: rowSession, ID: session.ID})
			}
			if visible < len(group.sessions) {
				rows = append(rows, sidebarRow{Kind: rowShowMore, ID: id, Project: group.project,
					Remaining: len(group.sessions) - visible})
			}
		}
		rows = append(rows, sidebarRow{Kind: rowSpacer, ID: id})
	}
	a.rows = rows
	a.fillSidebarBranches(a.projects)
}

func sessionStamp(session *model.Session) uint64 {
	if session.LastReplyAt != nil {
		return *session.LastReplyAt
	}
	return session.CreatedAt
}

func (a *app) sidebarGroupVisible(projectID string) int {
	if n := a.visible[projectID]; n > 0 {
		return n
	}
	return groupVisibleDefault
}

func sidebarGroupVisibleFor(index, total int) int {
	return min(max((index/groupVisibleStep+1)*groupVisibleStep, groupVisibleDefault), total)
}

func (a *app) sortedGroupSessions(projectID string) []*model.Session {
	var sessions []*model.Session
	for i := range a.sessions {
		session := &a.sessions[i]
		if session.ProjectID == projectID && !session.Archived && session.HasStarted() {
			sessions = append(sessions, session)
		}
	}
	sort.SliceStable(sessions, func(i, j int) bool {
		return sessionStamp(sessions[i]) > sessionStamp(sessions[j])
	})
	return sessions
}

func (a *app) revealSidebarSession(id string) {
	session := a.summarySession(id)
	if session == nil {
		return
	}
	projectID := session.ProjectID
	if a.collapsed == nil {
		a.collapsed = make(map[string]bool)
	}
	a.collapsed[projectID] = false
	sessions := a.sortedGroupSessions(projectID)
	for i, candidate := range sessions {
		if candidate.ID == id && i >= a.sidebarGroupVisible(projectID) {
			if a.visible == nil {
				a.visible = make(map[string]int)
			}
			a.visible[projectID] = sidebarGroupVisibleFor(i, len(sessions))
			break
		}
	}
	a.rebuildRows()
	for i, row := range a.rows {
		if row.Kind == rowSession && row.ID == id {
			a.sidebarList.ScrollIntoView(i)
			return
		}
	}
}
