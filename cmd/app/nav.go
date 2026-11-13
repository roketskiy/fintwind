package main

import (
	"fmt"
	"strings"

	"github.com/egoist/mygo/ui"
)

// Values from app.rs's navigation rail.
const (
	navRailWidth        = 44.0
	navRailLeft         = 16.0
	navRailContentGap   = 16.0
	navRailTickWidth    = 32.0
	navRailTickHeight   = 2.0
	navRailTickGap      = 10.0
	navRailTurnHeight   = navRailTickHeight + navRailTickGap
	navRailInactiveOpac = 0.75
	// Height of the rail's top/bottom fade masks, from transcript_view.rs
	// NAVIGATION_RAIL_FADE_HEIGHT.
	navRailFadeHeight = 20.0
	navPreviewWidth   = 320.0
	navPreviewHeight  = 126.0
)

type navTurn struct {
	Row      int
	ID       string
	Prompt   string
	Response string
}

// navigationTurns mirrors transcript_navigation_turns: one entry per user
// prompt, resolved to its transcript row, with the preview snippets.
func (a *app) navigationTurns() []navTurn {
	session := a.detail
	if session == nil {
		return nil
	}
	userIndexes := make([]int, 0, len(session.Messages))
	for index := range session.Messages {
		if session.Messages[index].Role == "user" {
			userIndexes = append(userIndexes, index)
		}
	}
	turns := make([]navTurn, 0, len(userIndexes))
	for turnIndex, messageIndex := range userIndexes {
		message := &session.Messages[messageIndex]
		row := -1
		for rowIndex := range a.transcriptRows {
			candidate := a.transcriptRows[rowIndex]
			if candidate.Kind == transcriptMessage && candidate.MessageIndex == messageIndex {
				row = rowIndex
				break
			}
		}
		if row < 0 {
			continue
		}
		nextUserIndex := len(session.Messages)
		if turnIndex+1 < len(userIndexes) {
			nextUserIndex = userIndexes[turnIndex+1]
		}
		turnRunning := false
		if message.TurnID != nil {
			for index := range session.Turns {
				if session.Turns[index].ID == *message.TurnID &&
					session.Turns[index].Status == "running" {
					turnRunning = true
					break
				}
			}
		}
		response := ""
		if !turnRunning {
			for index := nextUserIndex - 1; index > messageIndex; index-- {
				candidate := &session.Messages[index]
				if candidate.Role == "assistant" && strings.TrimSpace(candidate.Content) != "" {
					response = navPreviewSnippet(candidate.Content, 240)
					break
				}
			}
		}
		prompt := ""
		if strings.TrimSpace(message.Text()) == "" {
			names := make([]string, 0, len(message.Attachments))
			for _, attachment := range message.Attachments {
				names = append(names, attachment.Name)
			}
			prompt = strings.Join(names, ", ")
		} else {
			prompt = navPreviewSnippet(message.Text(), 100)
		}
		turns = append(turns, navTurn{Row: row, ID: message.ID, Prompt: prompt, Response: response})
	}
	return turns
}

// navPreviewSnippet collapses whitespace and caps the preview length, adding
// an ellipsis only when more text follows.
func navPreviewSnippet(content string, maxGraphemes int) string {
	normalized := strings.Join(strings.Fields(content), " ")
	runes := []rune(normalized)
	if len(runes) > maxGraphemes {
		return string(runes[:maxGraphemes]) + "…"
	}
	return string(runes)
}

// navigationRail draws the right-hand turn ticks when the viewport leaves room
// for the rail beside the transcript column.
func (a *app) navigationRail(c *ui.Context) {
	if a.detail == nil {
		return
	}
	turns := a.navigationTurns()
	if len(turns) < 2 {
		return
	}
	windowWidth, windowHeight := c.Size()
	chatWidth := windowWidth - modeRailWidth
	if a.sidebarOpen {
		chatWidth -= sidebarDefault
	}
	if a.rightOpen {
		chatWidth -= rightPanelWidth
	}
	contentLeft := (chatWidth - transcriptWidth) / 2
	if contentLeft < 20 {
		contentLeft = 20
	}
	if contentLeft < navRailLeft+navRailWidth+navRailContentGap {
		return
	}
	railHeight := float32(len(turns)) * navRailTurnHeight
	available := windowHeight - topbarHeight - 150
	if available < 120 {
		available = 120
	}
	if limit := available * 0.8; railHeight > limit {
		railHeight = limit
	}
	// The Go rail is fixed-height and never scrolls, so both fades share one
	// condition: the full tick stack exceeds the capped rail height. This
	// approximates transcript.rs navigation_rail_fade_visibility, which keys
	// each fade off the scroll offset and max offset instead.
	railCompressed := float32(len(turns))*navRailTurnHeight > railHeight
	if a.navHover < 0 || a.navHover >= len(turns) {
		a.navHover = -1
	}
	hovered := a.navHover
	a.navHover = -1
	// The active tick is the turn under the viewport's top edge, or the last
	// turn while the transcript rests at its end.
	activeTurn := len(turns) - 1
	if !a.messageList.AtEnd() {
		first, _ := a.messageList.Visible()
		activeTurn = 0
		for index := range turns {
			if turns[index].Row <= first {
				activeTurn = index
			}
		}
	}
	rail := ui.Column(c).Absolute().Left(navRailLeft).Top(0).Bottom(0).
		Width(navRailWidth).Justify(ui.Center)
	rail.Children(func() {
		ticks := ui.Column(c).Width(navRailWidth).Height(railHeight).Gap(0).
			AlignItems(ui.Start)
		ticks.Children(func() {
			for index := range turns {
				index := index
				turn := turns[index]
				scale := float32(0.25)
				if hovered >= 0 {
					switch diff := index - hovered; {
					case diff == 0:
						scale = 1.0
					case diff == 1 || diff == -1:
						scale = 0.68
					case diff == 2 || diff == -2:
						scale = 0.44
					}
				}
				color := alpha(palette.textSecondary, navRailInactiveOpac)
				if index == hovered || index == activeTurn {
					color = palette.text
				}
				if index == activeTurn && scale < 0.44 {
					scale = 0.44
				}
				hit := ui.Row(c).Width(navRailWidth).Height(navRailTurnHeight).
					AlignItems(ui.Center).Label(fmt.Sprintf(tr("Go to message %d"), index+1))
				hit.Children(func() {
					ui.Row(c).Width(navRailTickWidth * scale).Height(navRailTickHeight).
						Radius(1).Background(color)
				})
				if hit.Hovered() {
					a.navHover = index
				}
				if hit.Clicked() {
					a.messageList.ScrollTo(turn.Row, ui.Start)
				}
			}
			// Top/bottom fade masks over the ticks, mirroring
			// ConversationNavigationRail: 20px surface-to-transparent
			// gradients, pointer-transparent so the ticks stay clickable.
			if railCompressed {
				transparent := ui.RGBA(palette.surface.R, palette.surface.G, palette.surface.B, 0)
				ui.Row(c).Absolute().Left(0).Right(0).Top(0).Height(navRailFadeHeight).
					LinearGradient(ui.LinearGradient{From: palette.surface, To: transparent, Angle: 180}).
					PassThrough()
				ui.Row(c).Absolute().Left(0).Right(0).Bottom(0).Height(navRailFadeHeight).
					LinearGradient(ui.LinearGradient{From: transparent, To: palette.surface, Angle: 180}).
					PassThrough()
			}
			if a.navHover >= 0 {
				a.navPreview(c, turns[a.navHover], a.navHover, railHeight)
			}
		})
	})
}

// navPreview is the hovered turn's card: 320 wide, at most 126 tall, with the
// prompt in 14/20 semibold and the answer snippet in 13/20 below it.
func (a *app) navPreview(c *ui.Context, turn navTurn, index int, railHeight float32) {
	top := float32(index)*navRailTurnHeight + navRailTurnHeight/2 - navPreviewHeight/2
	limit := railHeight - navPreviewHeight
	if limit < 0 {
		limit = 0
	}
	if top < 0 {
		top = 0
	}
	if top > limit {
		top = limit
	}
	card := ui.Column(c).Absolute().Left(navRailWidth+navRailContentGap).Top(top).
		Width(navPreviewWidth).MaxHeight(navPreviewHeight).Clip().Radius(14).
		Border(1, palette.borderStrong).Background(palette.raised).Padding(12, 15).
		Gap(7).Justify(ui.Start)
	card = cardShadow(card)
	card.Children(func() {
		ui.Text(c, turn.Prompt).FontSize(uiPx(14)).FixedLineHeight(uiPx(20)).FontWeight(600).
			TextColor(palette.text).SingleLine().Ellipsis("…")
		if turn.Response != "" {
			ui.Text(c, turn.Response).FillWidth().FontSize(uiPx(13)).FixedLineHeight(uiPx(20)).
				TextColor(palette.textSecondary).MaxLines(3).Ellipsis("…")
		}
	})
}
