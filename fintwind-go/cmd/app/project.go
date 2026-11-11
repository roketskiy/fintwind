package main

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
	"github.com/google/uuid"
)

// revealProjectFolder opens the selected workspace in the system file manager.
// projectBranch reads the repository's current branch from its HEAD. It runs
// when a task loads, never from render, and handles worktrees whose .git is a
// pointer file.
func projectBranch(root string) string {
	if root == "" {
		return ""
	}
	gitPath := filepath.Join(root, ".git")
	head := ""
	if info, err := os.Stat(gitPath); err == nil && info.Mode().IsRegular() {
		if data, err := os.ReadFile(gitPath); err == nil {
			line := strings.TrimSpace(string(data))
			if rest, ok := strings.CutPrefix(line, "gitdir:"); ok {
				gitPath = strings.TrimSpace(rest)
				if !filepath.IsAbs(gitPath) {
					gitPath = filepath.Join(root, gitPath)
				}
				if data, err := os.ReadFile(filepath.Join(gitPath, "HEAD")); err == nil {
					head = strings.TrimSpace(string(data))
				}
			}
		}
	} else if data, err := os.ReadFile(filepath.Join(gitPath, "HEAD")); err == nil {
		head = strings.TrimSpace(string(data))
	}
	if head == "" {
		return ""
	}
	if ref, ok := strings.CutPrefix(head, "ref: refs/heads/"); ok {
		return ref
	}
	if strings.HasPrefix(head, "ref: ") {
		return strings.TrimPrefix(head, "ref: ")
	}
	return "detached HEAD"
}

type branchState struct {
	root       string
	label      string
	generation uint64
}

func (a *app) fillSidebarBranches(projects []model.Project) {
	for _, project := range projects {
		a.queueProjectBranch(project, false)
	}
}

func (a *app) queueProjectBranch(project model.Project, force bool) {
	if a.branches == nil {
		a.branches = make(map[string]branchState)
	}
	previous, exists := a.branches[project.ID]
	if !force && exists && previous.root == project.Path {
		return
	}
	state := branchState{root: project.Path, label: previous.label, generation: previous.generation + 1}
	a.branches[project.ID] = state
	local := project.EnvironmentID == "" || project.EnvironmentID == "00000000-0000-0000-0000-000000000000"
	if a.client != nil && project.EnvironmentID == a.client.Identity().EnvironmentID {
		local = true
	}
	go func() {
		label := ""
		if local {
			label = projectBranch(project.Path)
		}
		a.post(func() {
			current := a.branches[project.ID]
			if current.generation != state.generation || current.root != project.Path {
				return
			}
			current.label = label
			a.branches[project.ID] = current
			if selected := a.selectedProject(); selected != nil && selected.ID == project.ID {
				a.branchLabel = label
			}
		})
	}()
}

// refreshBranch caches the selected workspace's branch for the footer.
func (a *app) refreshBranch() {
	project := a.selectedProject()
	if project == nil || project.Name == "No project" {
		a.branchLabel = ""
		return
	}
	a.branchLabel = a.branches[project.ID].label
	a.queueProjectBranch(*project, true)
}

// createDraftSession opens a local draft for the selected project, matching
// the source's behaviour where a new task appears immediately. The draft is
// sent to the daemon on its first submission, so an abandoned draft leaves no
// record behind.
func (a *app) createDraftSession() bool {
	project := a.selectedProject()
	if project == nil {
		return false
	}
	now := uint64(time.Now().Unix())
	id := uuid.NewString()
	a.rememberSession()
	a.switchNativeSession(a.selectedID, id)
	a.resetSessionView()
	session := model.Session{
		ID:              id,
		Title:           "New task",
		ProjectID:       project.ID,
		Provider:        "opencode",
		RuntimeMode:     "fullAccess",
		InteractionMode: "build",
		Status:          "idle",
		CreatedAt:       now,
		UpdatedAt:       now,
		Draft:           true,
	}
	a.sessions = append(a.sessions, session)
	a.byID = nil
	a.lastProject = project.ID
	a.selectedID = id
	a.detail = &session
	a.ensureContextSources()
	a.restoreDraftContextWindow()
	a.syncSummary(a.detail)
	a.unsavedDraft = id
	a.addOpenTab(id)
	a.recordHistory(id)
	a.rebuildRows()
	a.rebuildTranscript()
	a.refreshBranch()
	a.messageList = ui.ListState{FollowEnd: true}
	a.rememberSession()
	return true
}

// openDraft binds the workspace to an already persisted draft session.
func (a *app) openDraft(id string) {
	session := a.summarySession(id)
	if session == nil {
		return
	}
	a.rememberSession()
	a.switchNativeSession(a.selectedID, id)
	a.resetSessionView()
	session.Draft = true
	copy := *session
	copy.Draft = true
	a.detail = &copy
	a.selectedID = id
	a.unsavedDraft = ""
	a.addOpenTab(id)
	a.recordHistory(id)
	a.rebuildTranscript()
	a.rebuildRows()
	a.refreshBranch()
	a.messageList = ui.ListState{FollowEnd: true}
	a.rememberSession()
}

func (a *app) revealProjectFolder() {
	project := a.selectedProject()
	if project == nil || project.Path == "" {
		return
	}
	_ = exec.Command("explorer.exe", project.Path).Start()
}

// pickProjectFolder opens the in-app Add Project dialog (Local flow). The
// native folder picker now lives behind the dialog's Browse button, matching
// the source's existing-folder flow without its remote/clone/create tabs.
func (a *app) pickProjectFolder() {
	a.openAddProject()
}

// addProject registers a project and its first draft session with the daemon.
// SaveTaskState merges, so sending only the new records cannot disturb tasks
// another client owns.
func (a *app) addProject(path string) {
	a.addProjectNamed(path, filepath.Base(path))
}

// createProjectless starts a task in the shared projectless workspace.
func (a *app) createProjectless() {
	if a.projectlessRoot == "" {
		return
	}
	a.addProjectNamed(a.projectlessRoot, "No project")
}
func (a *app) addProjectNamed(path, name string) {
	path = strings.TrimSpace(path)
	if path == "" || a.client == nil {
		addProjectBusy = false
		return
	}
	for index := range a.projects {
		if strings.EqualFold(a.projects[index].Path, path) {
			a.lastProject = a.projects[index].ID
			addProjectOpen = false
			addProjectBusy = false
			a.newTab()
			return
		}
	}
	now := uint64(time.Now().Unix())
	project := model.Project{
		ID:        uuid.NewString(),
		Name:      name,
		Path:      path,
		CreatedAt: now,
	}
	sessionID := uuid.NewString()
	session := map[string]any{
		"id":                     sessionID,
		"title":                  "New task",
		"project_id":             project.ID,
		"provider":               "opencode",
		"runtime_mode":           "fullAccess",
		"interaction_mode":       "build",
		"status":                 "idle",
		"created_at":             now,
		"updated_at":             now,
		"mermaid_prompt_version": "v4",
	}
	command := map[string]any{
		"type": "saveTaskState",
		"projects": []any{map[string]any{
			"id":             project.ID,
			"name":           project.Name,
			"path":           project.Path,
			"environment_id": "",
			"created_at":     project.CreatedAt,
		}},
		"liveSessionIds": []string{sessionID},
		"sessions":       []any{session},
	}
	client := a.client
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		if _, err := client.Request(ctx, command); err != nil {
			a.post(func() {
				addProjectBusy = false
				addProjectErr = err.Error()
				a.status = err.Error()
			})
			return
		}
		a.post(func() {
			addProjectBusy = false
			addProjectOpen = false
			a.projects = append(a.projects, project)
			a.sessions = append(a.sessions, model.Session{
				ID:              sessionID,
				Title:           "New task",
				ProjectID:       project.ID,
				Provider:        "opencode",
				RuntimeMode:     "fullAccess",
				InteractionMode: "build",
				Status:          "idle",
				CreatedAt:       now,
				UpdatedAt:       now,
			})
			a.byID = nil
			a.lastProject = project.ID
			a.rebuildRows()
			a.openDraft(sessionID)
		})
	}()
}
