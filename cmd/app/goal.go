package main

import (
	"context"
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
	"time"
	"unicode"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/action"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

func goalRevision(value any) uint64 {
	switch number := value.(type) {
	case json.Number:
		parsed, _ := strconv.ParseUint(string(number), 10, 64)
		return parsed
	case float64:
		if number > 0 {
			return uint64(number)
		}
	}
	return 0
}

func parseGoalCommand(text string) (map[string]any, bool) {
	text = strings.TrimSpace(text)
	command, argument := text, ""
	if index := strings.IndexFunc(text, unicode.IsSpace); index >= 0 {
		command, argument = text[:index], text[index:]
	}
	if !strings.EqualFold(command, "/goal") {
		return nil, false
	}
	argument = strings.TrimSpace(argument)
	switch strings.ToLower(argument) {
	case "":
		return map[string]any{"type": "get"}, true
	case "pause", "resume", "clear":
		return map[string]any{"type": strings.ToLower(argument)}, true
	default:
		return map[string]any{"type": "start", "objective": argument}, true
	}
}

func (a *app) controlGoal(session *model.Session, goalAction map[string]any, submission *sessionSubmission) bool {
	if session == nil || a.client == nil || a.connection.phase != "online" || a.sessionActionBlocksInput(session.ID) {
		return false
	}
	state := a.stateForSession(session)
	if state.goalBusy || state.admissionBusy || hasUnsettledSubmissionReceipts(session) && !session.Busy() {
		a.status = tr("Resolve the pending submission before changing this task")
		return false
	}
	if goalAction["type"] == "start" && session.Busy() {
		a.status = tr("Wait for the current turn to finish before starting a goal")
		return false
	}
	project := a.projectForID(session.ProjectID)
	if project == nil || project.Path == "" || !sessionProjectIsLocal(project) {
		a.status = tr("This task's workspace is unavailable on this device")
		return false
	}
	state.goalBusy = true
	draft := ""
	var attachments []draftAttachment
	var references []composerReference
	if submission != nil {
		draft = submission.text
		attachments = append(attachments, submission.attachments...)
		if a.detail == session {
			draft = a.draft
			references = append(references, a.pendingReferences...)
		}
	}
	client, sessionID := a.client, session.ID
	identity := client.Identity()
	launch := newRuntimeLaunch(session, project.Path)
	launch.mermaidVersion = a.sessionMermaidVersion(session)
	runtimeID := a.runtimes[sessionID]
	var admission *submissionAdmission
	if state.unsaved || a.unsavedDraft == sessionID {
		snapshot := a.buildSessionSnapshot(session, project)
		request, err := json.Marshal(snapshot)
		if err != nil {
			state.goalBusy = false
			a.status = tr("Could not save the session: ") + err.Error()
			return false
		}
		admission = &submissionAdmission{done: make(chan struct{}), sessionID: sessionID, request: request}
		delete(a.snapshots.pending, sessionID)
		a.snapshots.admissions = append(a.snapshots.admissions, admission)
		a.startNextSnapshot()
	}
	go func() {
		ctx, cancel := context.WithTimeout(a.lifetime, 120*time.Second)
		defer cancel()
		var err error
		sent := false
		if admission != nil {
			select {
			case <-admission.done:
				err = admission.err
				if err == nil {
					err = a.onUI(func() error {
						a.markSessionSaved(sessionID)
						return nil
					})
				}
			case <-ctx.Done():
				err = ctx.Err()
			}
		}
		if err == nil && runtimeID == "" {
			runtimeID, err = ensureRuntime(ctx, client, launch)
		}
		if err == nil {
			err = a.onUI(func() error {
				if a.client != client || client.Identity() != identity || a.eventSession(sessionID) == nil {
					return fmt.Errorf("session connection changed")
				}
				a.runtimes[sessionID] = runtimeID
				if current := a.eventSession(sessionID); current != nil && (goalAction["type"] == "start" || goalAction["type"] == "resume") {
					a.stateForSession(current).stopped = false
				}
				return nil
			})
		}
		if err == nil {
			sent = true
			_, err = client.RequestFor(ctx, sessionID, runtimeID, map[string]any{"type": "goal", "action": goalAction})
		}
		a.post(func() {
			state.goalBusy = false
			if err != nil {
				if !sent && submission != nil {
					if a.detail != nil && a.detail.ID == sessionID {
						if a.draft == "" && len(a.pendingAttachments) == 0 && len(a.pendingReferences) == 0 {
							a.draft, a.pendingAttachments, a.pendingReferences = draft, attachments, references
						}
					} else if state.draft == "" && len(state.attachments) == 0 && len(state.references) == 0 {
						state.draft, state.attachments, state.references = draft, attachments, references
					}
				}
				if a.selectedID == sessionID {
					a.status = tr("Goal command failed: ") + err.Error()
				}
			}
		})
	}()
	return true
}

func (a *app) composerGoal(c *ui.Context) {
	if a.detail == nil {
		return
	}
	session := a.detail
	state := a.stateForSession(session)
	goal, ok := a.rawSession(session)["goal"].(map[string]any)
	if !ok || goal == nil {
		return
	}
	status, _ := goal["status"].(string)
	if status == "done" || status == "complete" {
		return
	}
	objective, _ := goal["objective"].(string)
	reason, _ := goal["reason"].(string)
	label, glyph, color := tr("Goal active"), "target", palette.accentText
	switch status {
	case "paused", "blocked":
		label, glyph, color = tr("Goal paused"), "pause", palette.warningText
		if status == "blocked" {
			label = tr("Goal blocked")
		}
	}
	ui.Column(c).FillWidth().MaxWidth(transcriptWidth).Margin(0, ui.Auto).AlignItems(ui.Start).Children(func() {
		lobeThemed(c, func() {
			ui.Column(c).AlignItems(ui.Start).Gap(4).Children(func() {
				if state.goalExpanded {
					ui.Column(c).Width(320).MaxWidthPercent(100).Padding(10, 12).Gap(8).
						Background(palette.composer).Border(1, palette.cardBorder).Radius(8).Children(func() {
						ui.Scroll(c).FillWidth().MaxHeight(160).Children(func() {
							ui.Column(c).FillWidth().Gap(6).Children(func() {
								ui.Text(c, objective).FontSize(uiPx(12)).TextColor(palette.text).Wrap()
								if reason != "" {
									ui.Text(c, reason).FontSize(uiPx(11)).TextColor(palette.textSecondary).Wrap()
								}
							})
						})
						ui.Row(c).FillWidth().Gap(5).AlignItems(ui.Center).Children(func() {
							if maxTurns := goalRevision(goal["maxTurns"]); maxTurns > 0 {
								ui.Text(c, fmt.Sprintf("%d / %d", goalRevision(goal["turns"]), maxTurns)).FontSize(uiPx(11)).TextColor(palette.textTertiary)
							}
							ui.Row(c).Grow(1)
							if status == "active" || status == "paused" || status == "blocked" {
								kind, title := "pause", tr("Pause")
								if status != "active" {
									kind, title = "resume", tr("Resume")
								}
								a.goalActionButton(c, session, kind, title, state.goalBusy)
							}
							a.goalActionButton(c, session, "clear", tr("Clear"), state.goalBusy)
						})
					})
				}
				button := action.Button(c.Key("goal-summary"), "", action.ButtonProps{Variant: action.Default, Size: theme.Small})
				button.Label(label).Radius(10).Gap(5).Padding(0, 9).Background(palette.composer).Border(1, palette.cardBorder).AlignSelf(ui.Start)
				button.Children(func() {
					icon(c, glyph, 12, color)
					ui.Text(c, label).FontSize(uiPx(11)).TextColor(color)
					icon(c, choice(state.goalExpanded, "chevron-up", "chevron-down"), 10, palette.textTertiary)
				})
				button.OnClick(func() { state.goalExpanded = !state.goalExpanded })
			})
		}).AlignSelf(ui.Start)
	})
}

func (a *app) goalActionButton(c *ui.Context, session *model.Session, kind, label string, disabled bool) {
	button := action.Button(c.Key("goal-"+kind), label, action.ButtonProps{Variant: action.Text, Size: theme.Small})
	button.Disabled(disabled).OnClick(func() { a.controlGoal(session, map[string]any{"type": kind}, nil) })
}
