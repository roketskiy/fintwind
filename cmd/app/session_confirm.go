package main

import (
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

type sessionDeleteConfirmation struct {
	open      bool
	id, title string
	project   model.Project
	client    *daemon.Client
	identity  daemon.Identity
}

func (a *app) sessionDeleteConfirmLayer(c *ui.Context) {
	confirmation := a.sessionActions.confirmation
	if confirmation == nil {
		return
	}
	c.Key("session-delete-dialog")
	ui.Modal(c, &confirmation.open, func() {
		ui.Text(c, tr("Delete task?")).FontSize(uiPx(17)).FontWeight(600).TextColor(palette.text)
		ui.Text(c, confirmation.title).FontSize(uiPx(13)).TextColor(palette.text).Wrap()
		ui.Text(c, tr("This also deletes its linked provider conversation. Project files are kept.")).FontSize(uiPx(12)).TextColor(palette.textSecondary).Wrap()
		ui.Row(c).FillWidth().Justify(ui.End).Gap(8).Children(func() {
			componentTextButton(c, "session-delete-cancel", tr("Cancel"), false).OnClick(func() { a.sessionActions.confirmation = nil })
			componentTextButton(c, "session-delete-confirm", tr("Delete task"), true).OnClick(func() {
				a.sessionActions.confirmation = nil
				session := a.summarySession(confirmation.id)
				if session == nil || a.client != confirmation.client {
					a.status = tr("The task or connection changed. Select Delete again.")
					return
				}
				project := a.projectForID(session.ProjectID)
				if project == nil || *project != confirmation.project || (a.client != nil && a.client.Identity() != confirmation.identity) {
					a.status = tr("The workspace changed. Select Delete again.")
					return
				}
				a.deleteSessionConfirmed(confirmation.id)
			})
		})
	}).Width(420).Background(palette.raised).Border(1, palette.borderStrong)
	if !confirmation.open && a.sessionActions.confirmation == confirmation {
		a.sessionActions.confirmation = nil
	}
}
