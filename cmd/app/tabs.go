package main

import (
	"time"

	"github.com/egoist/mygo/ui"
)

const (
	sessionTabHeight   = 30.0
	sessionTabWidth    = 190.0
	sessionTabFadeSize = 16.0
	// sessionTabDragThreshold mirrors SESSION_TAB_DRAG_THRESHOLD: pointer
	// travel before a press starts reordering tabs, so a jittery click
	// never shuffles the strip.
	sessionTabDragThreshold = 4.0
	// sessionTabPitch is the fixed per-tab advance: width plus the strip gap.
	sessionTabPitch = sessionTabWidth + 2
)

func (a *app) tabRow(c *ui.Context) {
	ui.Row(c).FillWidth().Height(tabRowHeight).AlignItems(ui.Center).
		Children(func() {
			newTab := ui.ButtonBase(c).Label(tr("New task")).Size(28, 28).Radius(7).Center().
				Margin(0, 4, 0, 8).Background(palette.overlay)
			if newTab.Hovered() {
				newTab.Background(palette.overlayStrong)
			}
			if newTab.Pressed() {
				newTab.Background(palette.overlayStrong)
			}
			newTab.Children(func() {
				icon(c, "plus", 14, palette.textTertiary)
			})
			if newTab.Clicked() {
				a.newTab()
			}
			// The strip takes its content width and shrinks when the window
			// runs out; the filler after it owns the remaining header drag
			// space, exactly as the source's drag region does. The host is
			// the positioning context for the edge fades; the strip scrolls
			// its tabs horizontally inside it.
			host := ui.Row(c).Shrink(1).MinWidth(0).Height(tabRowHeight)
			host.Children(func() {
				strip := ui.ScrollHorizontal(c).Grow(1).MinWidth(0).Height(tabRowHeight).
					Gap(2).AlignItems(ui.Center).Scrollbars(false).TrackScroll(&a.tabStripScroll)
				strip.Children(func() {
					for _, id := range a.openTabs {
						a.sessionTab(c, id)
					}
					// Keeps the last tab clear of the right fade.
					ui.Row(c).Width(sessionTabFadeSize).Height(1).Shrink(0)
				})
				a.tabFade(c, true)
				a.tabFade(c, false)
			})
			ui.Row(c).MinWidth(12).Grow(1)
		})
}

// sessionTab draws one tab: 190x30, 8 insets, 7 radius, 6 gap.
func (a *app) sessionTab(c *ui.Context, sessionID string) {
	session := a.byID[sessionID]
	if session == nil {
		for index := range a.sessions {
			if a.sessions[index].ID == sessionID {
				session = &a.sessions[index]
				break
			}
		}
	}
	title := tr("New task")
	status := tr("idle")
	if session != nil {
		title = sidebarSessionTitle(session)
		status = a.sessionStatus(sessionID)
	}
	attention := ""
	if status == "failed" {
		attention = "failed"
	} else if a.sessionAttention[sessionID] == "question" {
		attention = "question"
	} else if a.sessionAttention[sessionID] == "permission" {
		attention = "permission"
	} else if status == "waiting" {
		attention = "waiting"
	}
	busy := attention == "" && (status == "connecting" || status == "working")
	unread := attention == "" && !busy && a.tabUnread[sessionID]
	active := sessionID == a.selectedID
	// Key keeps the tab's press/drag state attached to the session across
	// reorders, instead of the strip position. Sizes, colors, fonts and the
	// indicator order below are untouched.
	tab := ui.ButtonBase(c).Key(sessionID).Label(title).Height(sessionTabHeight).
		Width(sessionTabWidth).Padding(0, 8).Radius(7).Shrink(0).
		Gap(6).AlignItems(ui.Center)
	if active {
		tab.Background(palette.overlayStrong)
	} else {
		if tab.Hovered() {
			tab.Background(palette.overlay)
		}
		if tab.Pressed() {
			tab.Background(palette.overlayStrong)
		}
	}
	var closeTab ui.Element
	tab.Children(func() {
		switch attention {
		case "failed":
			icon(c, "alert", 12, palette.danger)
		case "question":
			icon(c, "message-square-text", 12, palette.warning)
		case "permission":
			icon(c, "lock", 12, palette.warning)
		case "waiting":
			icon(c, "alert", 12, palette.warning)
		default:
			if busy {
				icon(c, "loader-circle", 12, palette.accent)
			} else if unread {
				ui.Row(c).Size(7, 7).Shrink(0).Radius(4).Background(palette.accent)
			}
		}
		titleColor := palette.textSecondary
		if active {
			titleColor = palette.text
		}
		ui.Row(c).Grow(1).MinWidth(0).Children(func() {
			ui.Text(c, title).FontSize(uiPx(12)).TextColor(titleColor).
				SingleLine().Ellipsis("...")
		})
		closeTab = ui.ButtonBase(c).Label(tr("Close")).Size(18, 18).Radius(5).Center().
			Margin(0, -2, 0, -2)
		if closeTab.Hovered() {
			closeTab.Background(palette.overlayStrong)
		}
		if closeTab.Pressed() {
			closeTab.Opacity(0.7)
		}
		closeTab.Children(func() {
			icon(c, "x", 10, palette.textTertiary)
		})
	})
	// The close button is its own press target: hitting it must never
	// activate the tab, so it is handled first with an early return.
	if closeTab.Clicked() {
		a.closeTab(sessionID)
		return
	}
	// mygo has RightClicked but no MiddleClicked. A middle press is read
	// through PressedButton (2 = middle), closing on press-down like the
	// source's on_mouse_down(Middle). closeTab is idempotent, so the frames
	// while the button stays held are harmless.
	if button, pressed := tab.PressedButton(); pressed && button == 2 {
		a.closeTab(sessionID)
		return
	}
	if button, pressed := closeTab.PressedButton(); pressed && button == 2 {
		a.closeTab(sessionID)
		return
	}
	// A reorder consumes the frame: activation already happened or will
	// happen through Clicked, and a reorder must not cause anything else.
	if a.reorderTabFromDrag(tab, sessionID) {
		return
	}
	// ScrollIntoView only fires on the frame a tab becomes selected;
	// every frame would fight user scrolling.
	if active && a.tabRevealID != sessionID {
		tab.ScrollIntoView()
		a.tabRevealID = sessionID
	}
	if tab.Clicked() && sessionID != a.selectedID {
		a.openSession(sessionID)
	}
}

// tabFade overlays a 16px surface-to-transparent gradient at the strip edge
// when the strip is scrolled past it, mirroring tabs.rs tab_fade. The fade
// is pointer-transparent so the tabs under it stay clickable.
func (a *app) tabFade(c *ui.Context, left bool) {
	scrolled := a.tabStripScroll.X
	visible := scrolled > 0.5
	if !left {
		visible = a.tabStripScroll.MaxX-scrolled > 0.5
	}
	if !visible {
		return
	}
	transparent := ui.RGBA(palette.surface.R, palette.surface.G, palette.surface.B, 0)
	gradient := ui.LinearGradient{From: palette.surface, To: transparent, Angle: 90}
	if !left {
		gradient = ui.LinearGradient{From: transparent, To: palette.surface, Angle: 90}
	}
	fade := ui.Row(c).Absolute().Top(0).Bottom(0).Width(sessionTabFadeSize).
		LinearGradient(gradient).PassThrough()
	if left {
		fade.Left(0)
	} else {
		fade.Right(0)
	}
}

// reorderTabFromDrag accumulates the tab's per-frame primary-press travel
// (Element.Dragged, called on the tab) in a.tabDragDX and reorders a.openTabs
// once it passes the 4px threshold, mirroring session_tab_drag_mouse_move. It
// reports whether it reordered: the drag state is cleared then, so a continued
// hold steps tab by tab. Release and cancel clear the state with no other
// side effects; activation stays on Clicked.
func (a *app) reorderTabFromDrag(tab ui.Element, sessionID string) bool {
	dx, _, dragging := tab.Dragged()
	if !dragging {
		if _, _, ended := tab.DragEnded(); ended && a.tabDragID == sessionID {
			a.tabDragID = ""
			a.tabDragDX = 0
		}
		if tab.PressCanceled() && a.tabDragID == sessionID {
			a.tabDragID = ""
			a.tabDragDX = 0
		}
		return false
	}
	if a.tabDragID != sessionID {
		a.tabDragID = sessionID
		a.tabDragDX = 0
	}
	a.tabDragDX += dx
	travel := a.tabDragDX
	if travel < 0 {
		travel = -travel
	}
	if travel < sessionTabDragThreshold {
		return false
	}
	from := -1
	for i, id := range a.openTabs {
		if id == sessionID {
			from = i
			break
		}
	}
	if from < 0 {
		a.tabDragID = ""
		a.tabDragDX = 0
		return false
	}
	target, ok := tabDragTarget(from, len(a.openTabs), a.tabDragDX)
	if !ok {
		return false
	}
	id := a.openTabs[from]
	a.openTabs = append(a.openTabs[:from], a.openTabs[from+1:]...)
	a.openTabs = append(a.openTabs, "")
	copy(a.openTabs[target+1:], a.openTabs[target:])
	a.openTabs[target] = id
	a.tabDragID = ""
	a.tabDragDX = 0
	return true
}

// tabDragTarget ports drag_target_index to cumulative travel: tabs are fixed
// pitch (190 + 2 gap), the press is assumed at the tab center, and neighbor
// midpoints sit half a tab from each slot edge. Moving left, the first
// (leftmost) crossed midpoint wins; moving right, the furthest crossed one
// wins. It reports false when no boundary was crossed.
func tabDragTarget(from, count int, dx float32) (int, bool) {
	half := float32(sessionTabWidth / 2)
	pointer := half + dx
	if dx < 0 {
		for i := 0; i < from; i++ {
			mid := float32(i-from)*sessionTabPitch + half
			if pointer < mid {
				return i, true
			}
		}
		return from, false
	}
	if dx > 0 {
		target := from
		for i := from + 1; i < count; i++ {
			mid := float32(i-from)*sessionTabPitch + half
			if pointer > mid {
				target = i
			}
		}
		return target, target != from
	}
	return from, false
}

func (a *app) addOpenTab(sessionID string) {
	for _, id := range a.openTabs {
		if id == sessionID {
			return
		}
	}
	a.openTabs = append(a.openTabs, sessionID)
}

// closeTab selects the tab that took the removed one's place, else the one
// before it, matching tab_neighbor_after_close.
func (a *app) closeTab(sessionID string) {
	if a.tabDragID == sessionID {
		a.tabDragID = ""
		a.tabDragDX = 0
	}
	index := -1
	for i, id := range a.openTabs {
		if id == sessionID {
			index = i
			break
		}
	}
	if index < 0 {
		return
	}
	a.openTabs = append(a.openTabs[:index], a.openTabs[index+1:]...)
	// A draft that never sent a prompt has no record to keep.
	if draft := a.summarySession(sessionID); draft != nil && draft.Draft &&
		len(draft.Messages) == 0 && len(draft.Turns) == 0 {
		for i := range a.sessions {
			if a.sessions[i].ID == sessionID {
				a.sessions = append(a.sessions[:i], a.sessions[i+1:]...)
				a.byID = nil
				break
			}
		}
		if a.unsavedDraft == sessionID {
			a.unsavedDraft = ""
		}
		a.rebuildRows()
	}
	if sessionID != a.selectedID {
		return
	}
	a.rememberSession()
	next := ""
	if index < len(a.openTabs) {
		next = a.openTabs[index]
	} else if index > 0 && index-1 < len(a.openTabs) {
		next = a.openTabs[index-1]
	}
	if next != "" {
		a.openSession(next)
		return
	}
	a.advanceSessionGeneration()
	a.switchNativeSession(a.selectedID, "")
	a.resetSessionView()
	a.selectedID = ""
	a.detail = nil
	a.detailErr = ""
	a.detailLoading = false
	a.transcriptRows = nil
}

// newTab opens a fresh task in the selected project, or clears the workspace
// for the onboarding empty state when no project exists.
func (a *app) newTab() {
	a.rememberSession()
	a.switchNativeSession(a.selectedID, "")
	a.resetSessionView()
	a.advanceSessionGeneration()
	a.selectedID = ""
	a.detail = nil
	a.detailErr = ""
	a.detailLoading = false
	a.transcriptRows = nil
	if a.createDraftSession() {
		return
	}
	a.rebuildTranscript()
}

func formatTabTime(unix uint64) string {
	if unix == 0 {
		return ""
	}
	return time.Unix(int64(unix), 0).Format("3:04 PM")
}
