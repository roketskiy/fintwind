package main

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"fintwind-go/internal/backgroundview"
	"fintwind-go/internal/graph"
	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

const (
	capsuleRefreshInterval = 5 * time.Second
	// historyPageSize is one lazy history read; the daemon bounds a single
	// window at 200 commits and paged walks at maxHistoryCommits.
	historyPageSize   = 200
	maxHistoryCommits = 2000
)

// The task capsule above the composer: one projection of the selected
// session's workspace changes, plan and background work, in the shape
// background_work/capsule.rs renders. The collapsed trigger and every tab
// body read from capsuleState, so the two never disagree.

type capsuleTab int

const (
	capsuleChanges capsuleTab = iota
	capsuleTodo
	capsuleTasks
)

// capsuleTodos is the agent's newest plan list, parsed once and shared by
// the collapsed badge and the open tab.
type capsuleTodos struct {
	Items     []capsuleTodoItem
	Completed int
	Open      int
}

type capsuleTodoItem struct {
	Content string
	Status  capsuleTodoStatus
}

type capsuleTodoStatus int

const (
	todoPending capsuleTodoStatus = iota
	todoInProgress
	todoCompleted
	todoCancelled
)

func capsuleTodoStatusFromWire(status string) capsuleTodoStatus {
	normalized := strings.ToLower(strings.TrimSpace(status))
	normalized = strings.NewReplacer("-", "_", " ", "_").Replace(normalized)
	switch normalized {
	case "completed", "complete", "done", "success":
		return todoCompleted
	case "in_progress", "running", "active":
		return todoInProgress
	case "cancelled", "canceled", "skipped":
		return todoCancelled
	}
	return todoPending
}

// latestCapsuleTodos walks the loaded transcript backwards for the newest
// plan activity. OpenCode's todowrite sends the complete list on every call,
// so the newest one is the whole state; an empty list is a cleared list.
func latestCapsuleTodos(session *model.Session) *capsuleTodos {
	if session == nil {
		return nil
	}
	for blockIndex := len(session.TranscriptBlocks) - 1; blockIndex >= 0; blockIndex-- {
		activities := session.TranscriptBlocks[blockIndex].Activities
		for index := len(activities) - 1; index >= 0; index-- {
			activity := &activities[index]
			if activity.Kind != model.KindPlan || activity.Arguments == nil {
				continue
			}
			if todos := parseCapsuleTodos(*activity.Arguments); todos != nil {
				return todos
			}
		}
	}
	return nil
}

func parseCapsuleTodos(arguments string) *capsuleTodos {
	value, err := decodeJSONPossiblyWrapped(arguments)
	if err != nil {
		return nil
	}
	list, ok := capsuleTodoList(value).([]any)
	if !ok {
		return nil
	}
	items := make([]capsuleTodoItem, 0, len(list))
	for _, entry := range list {
		item, ok := entry.(map[string]any)
		if !ok {
			continue
		}
		content, ok := capsuleTodoContent(item).(string)
		if !ok || strings.TrimSpace(content) == "" {
			continue
		}
		items = append(items, capsuleTodoItem{
			Content: strings.TrimSpace(content),
			Status:  capsuleTodoItemStatus(item),
		})
	}
	// A non-empty array whose entries all failed to parse is not a todo
	// list, so the caller keeps looking further back.
	if len(list) > 0 && len(items) == 0 {
		return nil
	}
	todos := &capsuleTodos{Items: items}
	for _, item := range items {
		switch item.Status {
		case todoCompleted:
			todos.Completed++
		case todoPending, todoInProgress:
			todos.Open++
		}
	}
	return todos
}

// decodeJSONPossiblyWrapped parses tool arguments, which some providers wrap
// as a JSON string holding JSON.
func decodeJSONPossiblyWrapped(arguments string) (any, error) {
	var value any
	if err := json.Unmarshal([]byte(arguments), &value); err != nil {
		return nil, err
	}
	if text, ok := value.(string); ok {
		if err := json.Unmarshal([]byte(text), &value); err != nil {
			return nil, err
		}
	}
	return value, nil
}

func capsuleTodoList(value any) any {
	object, ok := value.(map[string]any)
	if !ok {
		return nil
	}
	for _, key := range []string{"todos", "plan", "items"} {
		if list, ok := object[key]; ok {
			return list
		}
	}
	return nil
}

func capsuleTodoContent(item map[string]any) any {
	for _, key := range []string{"content", "step", "title", "text"} {
		if content, ok := item[key]; ok {
			return content
		}
	}
	return nil
}

func capsuleTodoItemStatus(item map[string]any) capsuleTodoStatus {
	if status, ok := item["status"].(string); ok {
		return capsuleTodoStatusFromWire(status)
	}
	if completed, ok := item["completed"].(bool); ok && completed {
		return todoCompleted
	}
	return todoPending
}

// capsuleRef is one ref decoration on a commit: a branch or tag label, its
// remote kind and whether it names HEAD.
type capsuleRef struct {
	Label  string `json:"label"`
	Remote bool   `json:"remote"`
	Head   bool   `json:"head"`
	// Tip is the hover text the overflow chip carries; never on the wire.
	Tip string `json:"-"`
}

// capsuleCommit is one history entry the graph draws.
type capsuleCommit struct {
	Hash      string       `json:"hash"`
	ShortHash string       `json:"shortHash"`
	Subject   string       `json:"subject"`
	Author    string       `json:"author"`
	Timestamp uint64       `json:"timestamp"`
	Parents   []string     `json:"parents"`
	Refs      []capsuleRef `json:"refs"`
	Head      bool         `json:"isHead"`
	Pushed    bool         `json:"pushed"`
}

// historyRow is one commit with its laid-out graph row; the lane model is
// the port of MyGit's internal/graph in fintwind-go/internal/graph.
type historyRow struct {
	Commit capsuleCommit
	Row    graph.Row
}

// layoutHistory lays the commits' graph out once per fetch, so painting
// stays a read.
func layoutHistory(commits []capsuleCommit) []historyRow {
	rows := make([]historyRow, 0, len(commits))
	var layout graph.Layout
	for _, commit := range commits {
		rows = append(rows, historyRow{Commit: commit, Row: layout.Add(commit.Hash, commit.Parents)})
	}
	return rows
}

// capsuleState is the capsule's whole projection. Rendering reads only
// these caches; fetches fill them off-thread behind generation guards.
type capsuleState struct {
	Tab  capsuleTab
	Open bool

	Scope string

	Branch               string
	Additions, Deletions uint64
	ChangesAt            time.Time
	ChangesGen           uint64

	// The right panel's History surface: branch-filtered local history with
	// its lane graph, the chosen row and that commit's file stats.
	HistoryBranch      string
	HistoryRows        []historyRow
	HistoryAt          time.Time
	HistoryGen         uint64
	HistoryLoading     bool
	HistoryLoadingMore bool
	HistoryExhausted   bool
	HistoryErr         string
	HistorySel         int
	HistoryMenuOpen    bool
	HistoryList        ui.ListState
	HistoryBranches    []scBranch
	HistoryStats       capsuleCommitStats
	HistoryStatsErr    string
	HistoryStatsGen    uint64

	Todos    *capsuleTodos
	TodosKey string

	Items      []model.BackgroundWorkItem
	ItemsValid bool

	// The open background work surface: the selected item's key plus the
	// synthetic transcript view state it owns. AgentDirty marks a fold or
	// item change since the synthetic session was built.
	AgentKey        string
	AgentDirty      bool
	AgentSession    *model.Session
	AgentRows       []transcriptRow
	AgentList       ui.ListState
	AgentDisclosure map[int]int
}

func (a *app) capsuleScope() string {
	project := a.selectedProject()
	if project == nil || a.client == nil || project.Path == "" {
		return ""
	}
	return project.Path + "\x00" + a.selectedID
}

// capsuleEnsure rebinds the capsule to the selected session's workspace and
// refreshes the change totals on the same throttle the source control page
// uses. The history surface refreshes on its own ensure.
func (a *app) capsuleEnsure() {
	scope := a.capsuleScope()
	if scope != a.capsule.Scope {
		a.capsule.Scope = scope
		a.capsule.Branch = ""
		a.capsule.Additions, a.capsule.Deletions = 0, 0
		a.capsule.ChangesAt = time.Time{}
		a.capsule.HistoryRows = nil
		a.capsule.HistoryAt = time.Time{}
		a.capsule.HistoryErr = ""
		a.capsule.HistorySel = -1
		a.capsule.HistoryExhausted = false
		a.capsule.HistoryLoadingMore = false
		a.capsule.HistoryBranches = nil
		a.capsule.Todos = nil
		a.capsule.TodosKey = ""
		a.capsule.Items = nil
		a.capsule.ItemsValid = false
		a.closeBackgroundSurface()
	}
	if scope == "" {
		return
	}
	now := time.Now()
	if now.Sub(a.capsule.ChangesAt) >= capsuleRefreshInterval {
		a.capsule.ChangesAt = now
		a.capsuleRefreshChanges()
	}
}

// capsuleTodosFor returns the selected session's newest plan list, reparsed
// only after the transcript changed; a cache miss is one scan, never a scan
// per frame.
func (a *app) capsuleTodosFor() *capsuleTodos {
	session := a.detail
	if session == nil {
		return nil
	}
	key := fmt.Sprintf("%s/%d/%d/%d", session.ID, session.UpdatedAt, len(session.TranscriptBlocks), len(session.Messages))
	if key == a.capsule.TodosKey {
		return a.capsule.Todos
	}
	a.capsule.TodosKey = key
	a.capsule.Todos = latestCapsuleTodos(session)
	return a.capsule.Todos
}

func (a *app) capsuleItemsFor() []model.BackgroundWorkItem {
	if a.capsule.ItemsValid {
		return a.capsule.Items
	}
	a.capsule.ItemsValid = true
	if a.detail == nil {
		a.capsule.Items = nil
		return nil
	}
	a.capsule.Items = backgroundview.Items(a.extension(a.detail))
	return a.capsule.Items
}

// closeBackgroundSurface drops the open background work surface's view
// state; the synthetic session never touches the selected session.
func (a *app) closeBackgroundSurface() {
	a.capsule.AgentKey = ""
	a.capsule.AgentSession = nil
	a.capsule.AgentRows = nil
	a.capsule.AgentDisclosure = nil
	a.capsule.AgentList = ui.ListState{FollowEnd: true}
}

// capsuleRefreshChanges fetches the workspace's branch and working-tree
// totals; a transient failure keeps the numbers already on screen.
func (a *app) capsuleRefreshChanges() {
	scope := a.scScope()
	if scope.Client == nil {
		return
	}
	a.capsule.ChangesGen++
	gen := a.capsule.ChangesGen
	origin := a.capsule.Scope
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		branch := ""
		additions, deletions := uint64(0), uint64(0)
		_, root, err := scHydrate(ctx, scope)
		var current struct {
			Type   string  `json:"type"`
			Branch *string `json:"branch"`
		}
		if err == nil {
			err = scRequest(ctx, scope.Client, map[string]any{"type": "currentBranch", "cwd": root}, &current)
			if err == nil && current.Branch != nil {
				branch = *current.Branch
			}
		}
		var stats struct {
			Additions uint64 `json:"additions"`
			Deletions uint64 `json:"deletions"`
		}
		if err == nil {
			var sourceWire any = string(reviewSourceUncommitted)
			err = scRequest(ctx, scope.Client, map[string]any{"type": "collectReviewStats", "cwd": root, "source": sourceWire}, &stats)
			additions, deletions = stats.Additions, stats.Deletions
		}
		a.post(func() {
			if gen != a.capsule.ChangesGen || a.capsule.Scope != origin || err != nil {
				return
			}
			a.capsule.Branch = branch
			a.capsule.Additions, a.capsule.Deletions = additions, deletions
		})
	}()
}
