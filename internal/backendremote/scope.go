package backendremote

import (
	"errors"
	"sort"
	"time"

	"fintwind-go/internal/model"
)

const sessionWindow = 72 * time.Hour

type Scope struct {
	projects map[string]struct{}
}

func NewScope(projects []model.Project) Scope {
	allowed := make(map[string]struct{}, len(projects))
	for _, project := range projects {
		allowed[project.ID] = struct{}{}
	}
	return Scope{projects: allowed}
}

func (scope Scope) Project(projects []model.Project, id string) (*model.Project, error) {
	if _, ok := scope.projects[id]; !ok {
		return nil, errors.New("workspace is outside the remote activation")
	}
	for index := range projects {
		if projects[index].ID == id {
			return &projects[index], nil
		}
	}
	return nil, errors.New("workspace is no longer available")
}

func (scope Scope) Session(projects []model.Project, sessions []model.Session, id string, now time.Time) (*model.Session, error) {
	for index := range sessions {
		session := &sessions[index]
		if session.ID != id {
			continue
		}
		if _, err := scope.Project(projects, session.ProjectID); err != nil {
			return nil, err
		}
		if session.Archived || session.Updated().Before(now.Add(-sessionWindow)) {
			return nil, errors.New("session is outside the remote activation")
		}
		return session, nil
	}
	return nil, errors.New("session is no longer available")
}

func (scope Scope) Workspaces(projects []model.Project, selected string) map[string]any {
	workspaces := make([]map[string]any, 0, min(200, len(projects)))
	var current any
	for _, project := range projects {
		if _, ok := scope.projects[project.ID]; !ok {
			continue
		}
		if project.ID == selected {
			current = selected
		}
		if len(workspaces) < 200 {
			workspaces = append(workspaces, map[string]any{"id": project.ID, "name": limitText(project.Name, 256, false)})
		}
	}
	return map[string]any{"workspaces": workspaces, "selectedProjectId": current}
}

func (scope Scope) Sessions(projects []model.Project, sessions []model.Session, projectID string, now time.Time) (map[string]any, error) {
	if _, err := scope.Project(projects, projectID); err != nil {
		return nil, err
	}
	cutoff := now.Add(-sessionWindow)
	visible := make([]model.Session, 0)
	for _, session := range sessions {
		if session.ProjectID == projectID && !session.Archived && !session.Updated().Before(cutoff) {
			visible = append(visible, session)
		}
	}
	sort.SliceStable(visible, func(left, right int) bool { return visible[left].Updated().After(visible[right].Updated()) })
	result := make([]map[string]any, 0, min(200, len(visible)))
	for index := 0; index < min(200, len(visible)); index++ {
		session := &visible[index]
		result = append(result, map[string]any{"id": session.ID, "projectId": session.ProjectID, "title": limitText(session.DisplayTitle(), 256, false), "status": session.Status, "updatedAt": session.Updated().Unix()})
	}
	return map[string]any{"sessions": result, "cutoff": cutoff.Unix()}, nil
}
