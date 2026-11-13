package main

import (
	"encoding/json"
	"fmt"
	"sort"
	"strconv"
	"strings"
	"time"
)

// Scheduled task wire types. The daemon speaks camelCase with a tagged
// "kind"; only the fields the settings page reads
// and writes are kept.

type schedLaunch struct {
	Model           string  `json:"model"`
	RuntimeMode     string  `json:"runtimeMode"`
	InteractionMode string  `json:"interactionMode,omitempty"`
	ReasoningEffort *string `json:"reasoningEffort,omitempty"`
	ServiceTier     *string `json:"serviceTier,omitempty"`
	ContextWindow   *string `json:"contextWindow,omitempty"`
	AgentPreset     *string `json:"agentPreset,omitempty"`
}

type schedSchedule struct {
	Kind         string  `json:"kind"`
	IntervalSecs uint32  `json:"intervalSecs,omitempty"`
	AtMinutes    uint16  `json:"at,omitempty"`
	Weekdays     uint8   `json:"weekdays,omitempty"`
	Timezone     *string `json:"timezone,omitempty"`
	OnceAt       uint64  `json:"onceAt,omitempty"`
}

func (s *schedSchedule) UnmarshalJSON(data []byte) error {
	type wire struct {
		Kind         string          `json:"kind"`
		IntervalSecs uint32          `json:"intervalSecs"`
		At           json.RawMessage `json:"at"`
		Weekdays     uint8           `json:"weekdays"`
		Timezone     *string         `json:"timezone"`
	}
	var w wire
	if err := json.Unmarshal(data, &w); err != nil {
		return err
	}
	s.Kind, s.IntervalSecs, s.Weekdays, s.Timezone = w.Kind, w.IntervalSecs, w.Weekdays, w.Timezone
	switch w.Kind {
	case "every", "atTime":
		if len(w.At) != 0 {
			var minutes uint16
			if err := json.Unmarshal(w.At, &minutes); err != nil {
				return err
			}
			s.AtMinutes = minutes
		}
	case "once":
		if len(w.At) != 0 {
			var at uint64
			if err := json.Unmarshal(w.At, &at); err != nil {
				return err
			}
			s.OnceAt = at
		}
	}
	return nil
}

func (s schedSchedule) MarshalJSON() ([]byte, error) {
	switch s.Kind {
	case "every":
		return json.Marshal(map[string]any{"kind": "every", "intervalSecs": s.IntervalSecs})
	case "once":
		return json.Marshal(map[string]any{"kind": "once", "at": s.OnceAt})
	default:
		out := map[string]any{"kind": "atTime", "at": s.AtMinutes, "weekdays": s.Weekdays}
		if s.Timezone != nil {
			out["timezone"] = *s.Timezone
		}
		return json.Marshal(out)
	}
}

type schedTarget struct {
	Kind      string          `json:"kind"`
	ProjectID string          `json:"projectId,omitempty"`
	Launch    schedLaunch     `json:"launch,omitempty"`
	Workspace json.RawMessage `json:"workspace,omitempty"`
	SessionID string          `json:"sessionId,omitempty"`
}

func (t *schedTarget) UnmarshalJSON(data []byte) error {
	type wire struct {
		Kind      string          `json:"kind"`
		ProjectID string          `json:"projectId"`
		Launch    schedLaunch     `json:"launch"`
		Workspace json.RawMessage `json:"workspace"`
		SessionID string          `json:"sessionId"`
	}
	var w wire
	if err := json.Unmarshal(data, &w); err != nil {
		return err
	}
	t.Kind, t.ProjectID, t.Launch, t.Workspace, t.SessionID = w.Kind, w.ProjectID, w.Launch, w.Workspace, w.SessionID
	return nil
}

func (t schedTarget) MarshalJSON() ([]byte, error) {
	switch t.Kind {
	case "existingSession":
		return json.Marshal(map[string]any{"kind": "existingSession", "sessionId": t.SessionID})
	default:
		workspace := t.Workspace
		if len(workspace) == 0 {
			workspace = json.RawMessage(`{"kind":"local"}`)
		}
		return json.Marshal(map[string]any{
			"kind": "newSession", "projectId": t.ProjectID,
			"launch": t.Launch, "workspace": workspace,
		})
	}
}

type schedPending struct {
	ID        string  `json:"id"`
	DueAt     uint64  `json:"dueAt"`
	SessionID *string `json:"sessionId"`
	Preparing bool    `json:"preparing"`
}

type schedOutcome struct {
	Outcome      string  `json:"outcome"`
	SessionID    *string `json:"sessionId"`
	SubmissionID *string `json:"submissionId,omitempty"`
	Reason       string  `json:"reason"`
}

func (o *schedOutcome) UnmarshalJSON(data []byte) error {
	type wire struct {
		Outcome      string  `json:"outcome"`
		SessionID    *string `json:"sessionId"`
		SubmissionID *string `json:"submissionId"`
		Reason       string  `json:"reason"`
	}
	var w wire
	if err := json.Unmarshal(data, &w); err != nil {
		return err
	}
	o.Outcome, o.SessionID, o.Reason = w.Outcome, w.SessionID, w.Reason
	o.SubmissionID = w.SubmissionID
	return nil
}

type schedLast struct {
	DueAt   uint64       `json:"dueAt"`
	At      uint64       `json:"at"`
	Outcome schedOutcome `json:"outcome"`
}

type scheduledTask struct {
	ID        string        `json:"id"`
	Revision  uint64        `json:"revision"`
	Title     string        `json:"title"`
	Prompt    string        `json:"prompt"`
	Enabled   bool          `json:"enabled"`
	Schedule  schedSchedule `json:"schedule"`
	Target    schedTarget   `json:"target"`
	NextDueAt *uint64       `json:"nextDueAt"`
	CreatedAt uint64        `json:"createdAt"`
	UpdatedAt uint64        `json:"updatedAt"`
	Pending   *schedPending `json:"pending"`
	Last      *schedLast    `json:"last"`
	RunCount  uint64        `json:"runCount"`
}

// schedTaskState mirrors TaskState in src/app/scheduled.rs.
func schedTaskState(task scheduledTask) string {
	if task.Pending != nil {
		if task.Pending.Preparing {
			return "preparing"
		}
		return "queued"
	}
	if task.Last == nil {
		return "never"
	}
	switch task.Last.Outcome.Outcome {
	case "accepted":
		return "dispatched"
	case "unconfirmed":
		return "unconfirmed"
	case "notSent":
		return "notSent"
	case "rejected":
		return "rejected"
	default:
		return "missed"
	}
}

func schedStateLabel(state string) string {
	switch state {
	case "queued":
		return tr("Queued")
	case "preparing":
		return tr("Preparing")
	case "dispatched":
		return tr("Dispatched")
	case "unconfirmed":
		return tr("Unconfirmed")
	case "notSent":
		return tr("Not sent")
	case "rejected":
		return tr("Rejected")
	case "missed":
		return tr("Skipped")
	default:
		return tr("Never run")
	}
}

func schedOutcomeReason(task scheduledTask) string {
	if task.Last == nil {
		return ""
	}
	switch task.Last.Outcome.Outcome {
	case "notSent", "rejected":
		return task.Last.Outcome.Reason
	default:
		return ""
	}
}

func schedLastSession(task scheduledTask) string {
	if task.Pending != nil && task.Pending.SessionID != nil {
		return *task.Pending.SessionID
	}
	if task.Last != nil && task.Last.Outcome.SessionID != nil {
		return *task.Last.Outcome.SessionID
	}
	return ""
}

func schedParseClock(text string) (uint16, bool) {
	parts := strings.Split(strings.TrimSpace(text), ":")
	if len(parts) != 2 {
		return 0, false
	}
	hour, err1 := strconv.Atoi(strings.TrimSpace(parts[0]))
	minute, err2 := strconv.Atoi(strings.TrimSpace(parts[1]))
	if err1 != nil || err2 != nil || hour < 0 || hour > 23 || minute < 0 || minute > 59 {
		return 0, false
	}
	return uint16(hour*60 + minute), true
}

func schedFormatClock(minutes uint16) string {
	return fmt.Sprintf("%02d:%02d", minutes/60, minutes%60)
}

func schedParseOnce(date, clock string) (uint64, bool) {
	clockMinutes, ok := schedParseClock(clock)
	if !ok {
		return 0, false
	}
	moment, err := time.ParseInLocation("2006-01-02 15:04", strings.TrimSpace(date)+" "+schedFormatClock(clockMinutes), time.Local)
	if err != nil {
		return 0, false
	}
	if moment.Unix() < 0 {
		return 0, false
	}
	return uint64(moment.Unix()), true
}

func schedFormatTimestamp(seconds uint64) string {
	return time.Unix(int64(seconds), 0).In(time.Local).Format("01-02 15:04")
}

func schedFormatOnce(seconds uint64) string {
	return time.Unix(int64(seconds), 0).In(time.Local).Format("2006-01-02 15:04")
}

var schedWeekdayOrder = []struct {
	bit   uint8
	label string
}{
	{1, "Mon"}, {2, "Tue"}, {3, "Wed"}, {4, "Thu"}, {5, "Fri"}, {6, "Sat"}, {0, "Sun"},
}

func schedWeekdayLabels(weekdays uint8) string {
	if weekdays == 127 {
		return tr("Every day")
	}
	var out []string
	for _, day := range schedWeekdayOrder {
		if weekdays&(1<<day.bit) != 0 {
			out = append(out, tr(day.label))
		}
	}
	return strings.Join(out, " ")
}

func schedScheduleSummary(schedule schedSchedule) string {
	switch schedule.Kind {
	case "every":
		if schedule.IntervalSecs%3600 == 0 {
			return strings.ReplaceAll(tr("Every %{count} h"), "%{count}", fmt.Sprint(schedule.IntervalSecs/3600))
		}
		minutes := schedule.IntervalSecs / 60
		if minutes < 1 {
			minutes = 1
		}
		return strings.ReplaceAll(tr("Every %{count} min"), "%{count}", fmt.Sprint(minutes))
	case "once":
		return strings.ReplaceAll(tr("Once %{time}"), "%{time}", schedFormatOnce(schedule.OnceAt))
	default:
		return strings.ReplaceAll(strings.ReplaceAll(tr("%{days} at %{time}"), "%{days}", schedWeekdayLabels(schedule.Weekdays)), "%{time}", schedFormatClock(schedule.AtMinutes))
	}
}

func (a *app) schedTargetLabel(task scheduledTask) string {
	switch task.Target.Kind {
	case "existingSession":
		for i := range a.sessions {
			if a.sessions[i].ID == task.Target.SessionID {
				title := a.sessions[i].DisplayTitle()
				if title == "" {
					title = a.sessions[i].ID
				}
				return title
			}
		}
		return tr("Unavailable conversation")
	default:
		for i := range a.projects {
			if a.projects[i].ID == task.Target.ProjectID {
				if a.projects[i].Name != "" {
					return a.projects[i].Name
				}
				return a.projects[i].ID
			}
		}
		return tr("Unavailable project")
	}
}

func schedSortTasks(tasks []scheduledTask) {
	sort.Slice(tasks, func(i, j int) bool {
		if tasks[i].Enabled != tasks[j].Enabled {
			return tasks[i].Enabled && !tasks[j].Enabled
		}
		var di, dj uint64
		hasI, hasJ := tasks[i].NextDueAt != nil, tasks[j].NextDueAt != nil
		if hasI {
			di = *tasks[i].NextDueAt
		}
		if hasJ {
			dj = *tasks[j].NextDueAt
		}
		if hasI != hasJ {
			return hasJ
		}
		if hasI && hasJ && di != dj {
			return di < dj
		}
		if tasks[i].Title != tasks[j].Title {
			return tasks[i].Title < tasks[j].Title
		}
		return tasks[i].ID < tasks[j].ID
	})
}

// schedVisibleKeys precomputes the lowercase search haystacks, mirroring
// refresh_task_keys in scheduled.rs: title + prompt + target label.
func (a *app) schedVisible(tasks []scheduledTask, filter, query string) ([]scheduledTask, []string) {
	tokens := strings.Fields(strings.ToLower(strings.TrimSpace(query)))
	visible := make([]scheduledTask, 0, len(tasks))
	keys := make([]string, 0, len(tasks))
	for _, task := range tasks {
		if filter == "active" && !task.Enabled {
			continue
		}
		if filter == "paused" && task.Enabled {
			continue
		}
		key := strings.ToLower(task.Title + " " + task.Prompt + " " + a.schedTargetLabel(task))
		matched := true
		for _, token := range tokens {
			if !strings.Contains(key, token) {
				matched = false
				break
			}
		}
		if !matched {
			continue
		}
		visible = append(visible, task)
		keys = append(keys, key)
	}
	return visible, keys
}

func schedAccessMode(mode string) string {
	switch mode {
	case "plan", "ask":
		return "ask"
	case "autoAcceptEdits":
		return "autoAcceptEdits"
	default:
		return "fullAccess"
	}
}

func schedAccessOptions() []settingsOption {
	return []settingsOption{
		{label: "Supervised", value: "ask"},
		{label: "Auto-accept edits", value: "autoAcceptEdits"},
		{label: "Full access", value: "fullAccess"},
	}
}

// schedDraft validates the settings form the way Editor::draft does and
// builds the daemon payload. projectIDs and sessionIDs scope the target
// pickers; models are free text so an unlisted model still saves.
func schedDraft(title, prompt, schedule, clock string, weekdays uint8, date, interval, target, projectID, sessionID, model, access string, enabled bool, timezone *string, workspace json.RawMessage) (map[string]any, string) {
	if strings.TrimSpace(title) == "" {
		return nil, tr("Enter a name")
	}
	if len(title) > 512 {
		return nil, tr("Enter a name")
	}
	if strings.TrimSpace(prompt) == "" {
		return nil, tr("Enter a prompt")
	}
	if len(prompt) > 1024*1024 {
		return nil, tr("Enter a prompt")
	}
	draft := map[string]any{"title": strings.TrimSpace(title), "prompt": strings.TrimSpace(prompt), "enabled": enabled}
	var sched map[string]any
	switch schedule {
	case "every":
		minutes, err := strconv.Atoi(strings.TrimSpace(interval))
		if err != nil || minutes < 1 || minutes > 525600 {
			return nil, tr("Interval must be 1 to 525600 minutes")
		}
		secs := uint32(minutes) * 60
		if secs < 60 || secs > 365*24*60*60 {
			return nil, tr("Interval must be 1 to 525600 minutes")
		}
		sched = map[string]any{"kind": "every", "intervalSecs": secs}
	case "once":
		at, ok := schedParseOnce(date, clock)
		if !ok {
			return nil, tr("Use YYYY-MM-DD and HH:MM")
		}
		if enabled && at <= uint64(time.Now().Unix()) {
			return nil, tr("Use a future time")
		}
		sched = map[string]any{"kind": "once", "at": at}
	default:
		minutes, ok := schedParseClock(clock)
		if !ok {
			return nil, tr("Use HH:MM")
		}
		if weekdays == 0 || weekdays > 127 {
			return nil, tr("Select at least one day")
		}
		at := map[string]any{"kind": "atTime", "at": minutes, "weekdays": weekdays}
		if timezone != nil {
			at["timezone"] = *timezone
		}
		sched = at
	}
	draft["schedule"] = sched
	switch target {
	case "existing":
		if strings.TrimSpace(sessionID) == "" {
			return nil, tr("Select a session")
		}
		draft["target"] = map[string]any{"kind": "existingSession", "sessionId": sessionID}
	default:
		if strings.TrimSpace(projectID) == "" {
			return nil, tr("Select a project")
		}
		if strings.TrimSpace(model) == "" {
			return nil, tr("Enter a model")
		}
		workspacePayload := workspace
		if len(workspacePayload) == 0 {
			workspacePayload = json.RawMessage(`{"kind":"local"}`)
		}
		var workspaceValue any
		if err := json.Unmarshal(workspacePayload, &workspaceValue); err != nil {
			workspaceValue = map[string]any{"kind": "local"}
		}
		draft["target"] = map[string]any{
			"kind": "newSession", "projectId": projectID,
			"launch":    map[string]any{"model": strings.TrimSpace(model), "runtimeMode": schedAccessMode(access)},
			"workspace": workspaceValue,
		}
	}
	return draft, ""
}
