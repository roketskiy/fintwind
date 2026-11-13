package main

import (
	"context"
	"strings"
	"time"
	"unicode/utf8"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/action"
	"github.com/lobehub/lobe-mygo/pkg/field"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

// Sidebar context-menu and inline-edit actions: session rename/remove and
// project removal. The renderers stay in sidebar.go; this file owns
// the behaviour so the menu wiring stays thin.

// beginSessionRename swaps one session card's title for the inline editor,
// seeded with the title the row shows.
func (a *app) beginSessionRename(id string) {
	session := a.summarySession(id)
	if session == nil {
		return
	}
	a.renamingSession = id
	a.renameDraft = sidebarSessionTitle(session)
	a.renameSelectAll = true
}

// A provider-backed title changes only after the native rename is confirmed.
func (a *app) commitSessionRename() {
	id := a.renamingSession
	if id == "" {
		return
	}
	a.renamingSession = ""
	title := strings.TrimSpace(a.renameDraft)
	a.renameDraft = ""
	target := a.summarySession(id)
	if target == nil || title == "" || title == target.DisplayTitle() {
		return
	}
	now := uint64(time.Now().Unix())
	if target.Draft {
		target.Title, target.UpdatedAt = title, now
		if current := a.eventSession(id); current != nil {
			current.Title, current.UpdatedAt = title, now
		}
		a.rebuildRows()
		return
	}
	a.persistRenamedSession(id, title, now)
}

func (a *app) cancelSessionRename() {
	if a.renamingSession == "" {
		return
	}
	a.renamingSession = ""
	a.renameDraft = ""
	a.focusComposerOnce = true
}

func (a *app) persistRenamedSession(id, title string, updatedAt uint64) {
	if operation := a.beginSessionOperation(id, "rename"); operation != nil {
		go a.runSessionOperation(id, title, updatedAt, operation)
	}
}

// The title editor owns Enter and Escape. Leaving the card commits the edit.
func (a *app) sessionRenameCell(c *ui.Context) {
	ui.Row(c.Key("session-rename:" + a.renamingSession)).Grow(1).MinWidth(0).Children(func() {
		input := field.Input(c, &a.renameDraft, field.InputProps{Size: theme.Small}).
			Label(tr("Rename session")).FontSize(uiPx(13.5)).TextColor(palette.text).
			AutoFocus().OnSubmit(a.commitSessionRename).
			OnShortcut(0, ui.KeyEscape, a.cancelSessionRename)
		if a.renameSelectAll {
			input.SetTextSelection(0, utf8.RuneCountInString(a.renameDraft))
			a.renameSelectAll = false
		}
	})
}

// Only this explicit delete action may remove the native conversation. The
// app record remains until both operations have confirmed acknowledgements.
func (a *app) removeSession(id string) {
	session := a.summarySession(id)
	if session == nil {
		return
	}
	project := a.projectForID(session.ProjectID)
	if project == nil {
		return
	}
	confirmation := &sessionDeleteConfirmation{open: true, id: id, title: session.DisplayTitle(), project: *project, client: a.client}
	if a.client != nil {
		confirmation.identity = a.client.Identity()
	}
	a.sessionActions.confirmation = confirmation
}

func (a *app) deleteSessionConfirmed(id string) {
	if id == "" {
		return
	}
	if target := a.summarySession(id); target != nil && target.Draft {
		a.removeSessionLocal(id)
		return
	}
	if operation := a.beginSessionOperation(id, "delete"); operation != nil {
		go a.runSessionOperation(id, "", 0, operation)
	}
}

func (a *app) removeSessionLocal(id string) {
	tabIndex := -1
	for index, open := range a.openTabs {
		if open == id {
			tabIndex = index
			break
		}
	}
	if tabIndex >= 0 {
		a.openTabs = append(a.openTabs[:tabIndex], a.openTabs[tabIndex+1:]...)
	}
	for index := range a.sessions {
		if a.sessions[index].ID == id {
			a.sessions = append(a.sessions[:index], a.sessions[index+1:]...)
			break
		}
	}
	a.byID = nil
	delete(a.tabUnread, id)
	delete(a.sessionAttention, id)
	delete(a.runtimes, id)
	delete(a.live, id)
	a.forgetDiagrams(id)
	if a.unsavedDraft == id {
		a.unsavedDraft = ""
	}
	if a.renamingSession == id {
		a.renamingSession = ""
		a.renameDraft = ""
	}
	a.rebuildRows()
	if id != a.selectedID {
		return
	}
	next := ""
	if tabIndex >= 0 {
		if tabIndex < len(a.openTabs) {
			next = a.openTabs[tabIndex]
		} else if tabIndex > 0 {
			next = a.openTabs[tabIndex-1]
		}
	}
	if next == "" {
		next = a.latestSessionID()
	}
	if next != "" {
		a.openSession(next)
		return
	}
	a.advanceSessionGeneration()
	a.selectedID = ""
	a.detail = nil
	a.detailErr = ""
	a.detailLoading = false
	a.transcriptRows = nil
}

// confirmProjectRemove deletes the confirmed project on the daemon, then
// drops its local record and sessions. Only the catalog entry goes away;
// folders on disk and provider history stay.
func (a *app) confirmProjectRemove() {
	id := a.projectRemoveConfirm
	if id == "" {
		return
	}
	a.projectRemoveConfirm = ""
	client := a.client
	if client == nil {
		a.removeProjectLocal(id)
		return
	}
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		if _, err := client.Request(ctx, map[string]any{
			"type":      "removeProject",
			"projectId": id,
		}); err != nil {
			a.post(func() { a.status = tr("Cannot remove project: ") + err.Error() })
			return
		}
		a.post(func() { a.removeProjectLocal(id) })
	}()
}

func (a *app) removeProjectLocal(id string) {
	if a.projectRemoveConfirm == id {
		a.projectRemoveConfirm = ""
	}
	removed := map[string]bool{}
	kept := a.sessions[:0]
	for _, session := range a.sessions {
		if session.ProjectID == id {
			removed[session.ID] = true
			continue
		}
		kept = append(kept, session)
	}
	a.sessions = kept
	a.byID = nil
	keptTabs := a.openTabs[:0]
	for _, open := range a.openTabs {
		if removed[open] {
			continue
		}
		keptTabs = append(keptTabs, open)
	}
	a.openTabs = keptTabs
	for sessionID := range removed {
		delete(a.tabUnread, sessionID)
		delete(a.sessionAttention, sessionID)
		delete(a.runtimes, sessionID)
		if a.unsavedDraft == sessionID {
			a.unsavedDraft = ""
		}
		if a.renamingSession == sessionID {
			a.renamingSession = ""
			a.renameDraft = ""
		}
	}
	for index := range a.projects {
		if a.projects[index].ID == id {
			a.projects = append(a.projects[:index], a.projects[index+1:]...)
			break
		}
	}
	delete(a.collapsed, id)
	delete(a.visible, id)
	if a.lastProject == id {
		a.lastProject = ""
		if len(a.projects) > 0 {
			a.lastProject = a.projects[0].ID
		}
	}
	a.rebuildRows()
	a.refreshBranch()
	if a.summarySession(a.selectedID) == nil {
		next := a.latestSessionID()
		if next != "" {
			a.openSession(next)
			return
		}
		a.advanceSessionGeneration()
		a.selectedID = ""
		a.detail = nil
		a.detailErr = ""
		a.detailLoading = false
		a.transcriptRows = nil
	}
}

func (a *app) projectRemoveConfirmBar(c *ui.Context, project *model.Project) {
	strip := ui.Column(c.Key("remove-project-confirmation")).FillWidth().Margin(2, 0, 0, 0).
		Padding(8, 10).Gap(6).Radius(8).Background(palette.raised).Border(1, palette.borderStrong)
	strip.OnShortcut(0, ui.KeyEscape, func() { a.projectRemoveConfirm = "" })
	strip.Children(func() {
		ui.Text(c, tr("Remove “")+project.Name+tr("” from the app?")).
			FontSize(uiPx(12.5)).FontWeight(600).TextColor(palette.text).Wrap()
		ui.Row(c).FillWidth().Gap(6).Justify(ui.End).Children(func() {
			action.Button(c.Key("cancel"), tr("Cancel"), action.ButtonProps{Variant: action.Text, Size: theme.Small}).
				OnClick(func() { a.projectRemoveConfirm = "" })
			action.Button(c.Key("remove"), tr("Remove"), action.ButtonProps{Variant: action.Default, Size: theme.Small}).
				TextColor(palette.danger).OnClick(a.confirmProjectRemove)
		})
	})
}
