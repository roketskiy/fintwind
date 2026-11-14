package main

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"time"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

const (
	capsuleRefreshInterval = 5 * time.Second
	historyCommitLimit     = 500
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

// capsuleGraphLine is one drawn connection. Without Dot the line flows from
// the From lane's top edge to the To lane's bottom edge; with Dot it leaves
// the commit's dot halfway down the row, curving into the To lane.
type capsuleGraphLine struct {
	From, To int
	Color    int
	Dot      bool
}

// capsuleGraphRow is one commit with its lane geometry, laid out once per
// history load so painting stays a read. FlowDown says a parent flows out
// of the commit's lane below the dot, so the painter draws that segment.
type capsuleGraphRow struct {
	Commit   capsuleCommit
	Lane     int
	Color    int
	LaneSpan int
	FlowDown bool
	Lines    []capsuleGraphLine
}

type capsuleLaneSlot struct {
	Hash  string
	Color int
}

// layoutCapsuleGraph assigns lanes and connections newest-first, the way
// MyGit's logview draws `git log --graph`: every commit flows in one lane,
// its first parent inherits that lane, further parents open lanes to the
// right or merge into lanes already flowing toward them, and freed lanes
// are reused before new ones open.
func layoutCapsuleGraph(commits []capsuleCommit) []capsuleGraphRow {
	rows := make([]capsuleGraphRow, 0, len(commits))
	var lanes []capsuleLaneSlot
	nextColor := 0
	freeLane := func() (int, bool) {
		for index, slot := range lanes {
			if slot.Hash == "" {
				return index, true
			}
		}
		return 0, false
	}
	laneOf := func(hash string) int {
		for index, slot := range lanes {
			if slot.Hash == hash {
				return index
			}
		}
		return -1
	}
	for _, commit := range commits {
		lane := laneOf(commit.Hash)
		if lane < 0 {
			if reused, ok := freeLane(); ok {
				lane = reused
			} else {
				lane = len(lanes)
				lanes = append(lanes, capsuleLaneSlot{})
			}
			lanes[lane] = capsuleLaneSlot{Hash: commit.Hash, Color: nextColor}
			nextColor++
		}
		color := lanes[lane].Color
		row := capsuleGraphRow{Commit: commit, Lane: lane, Color: color, LaneSpan: len(lanes)}
		for index, slot := range lanes {
			if index != lane && slot.Hash != "" {
				row.Lines = append(row.Lines, capsuleGraphLine{From: index, To: index, Color: slot.Color})
			}
		}
		// Retire the commit. The first parent keeps the lane flowing when it
		// is not already flowing elsewhere; the rest merge or open lanes.
		lanes[lane] = capsuleLaneSlot{Color: color}
		for parentIndex, parent := range commit.Parents {
			if parent == "" {
				continue
			}
			if existing := laneOf(parent); existing >= 0 {
				row.Lines = append(row.Lines, capsuleGraphLine{From: lane, To: existing, Color: color, Dot: true})
				continue
			}
			target := lane
			if parentIndex > 0 {
				if reused, ok := freeLane(); ok {
					target = reused
				} else {
					target = len(lanes)
					lanes = append(lanes, capsuleLaneSlot{})
				}
			}
			lanes[target] = capsuleLaneSlot{Hash: parent, Color: color}
			if target != lane {
				row.Lines = append(row.Lines, capsuleGraphLine{From: lane, To: target, Color: color, Dot: true})
			}
		}
		row.FlowDown = lanes[lane].Hash != ""
		rows = append(rows, row)
	}
	return rows
}

// capsuleBackgroundItems decodes the session's background work projection
// into ordered items. The fold stores raw maps keyed by kind and provider,
// so the decode sorts by start time to keep rows stable across frames.
func capsuleBackgroundItems(ext map[string]any) []model.BackgroundWorkItem {
	raw, _ := ext["backgroundItems"].(map[string]any)
	items := make([]model.BackgroundWorkItem, 0, len(raw))
	for key, value := range raw {
		encoded, err := json.Marshal(value)
		if err != nil {
			continue
		}
		var item model.BackgroundWorkItem
		if json.Unmarshal(encoded, &item) != nil || item.Key.ProviderID == "" {
			continue
		}
		if item.Key.Kind == "" {
			if cut := strings.IndexByte(key, '/'); cut > 0 {
				item.Key.Kind = key[:cut]
			}
		}
		items = append(items, item)
	}
	sort.SliceStable(items, func(i, j int) bool {
		if items[i].StartedAtMs != items[j].StartedAtMs {
			return items[i].StartedAtMs < items[j].StartedAtMs
		}
		return items[i].Key.ProviderID < items[j].Key.ProviderID
	})
	return items
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
	HistoryBranch   string
	HistoryRows     []capsuleGraphRow
	HistoryAt       time.Time
	HistoryGen      uint64
	HistoryLoading  bool
	HistoryErr      string
	HistorySel      int
	HistoryMenuOpen bool
	HistoryList     ui.ListState
	HistoryBranches []scBranch
	HistoryStats    capsuleCommitStats
	HistoryStatsErr string
	HistoryStatsGen uint64

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
	a.capsule.Items = capsuleBackgroundItems(a.extension(a.detail))
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
