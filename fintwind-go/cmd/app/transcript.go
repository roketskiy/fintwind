package main

import (
	"fmt"
	"strings"
	"time"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/transfer"
	"github.com/egoist/mygo/ui"
)

func (a *app) sessionsView(c *ui.Context) {
	// Window-level overlay, so position in the tree does not matter.
	a.addProjectDialog(c)
	root := ui.Column(c).Grow(1).MinWidth(0).FillHeight().Background(palette.surface).
		Radius(workspaceRadius, 0, 0, workspaceRadius).Clip()
	root = workspaceShadow(root)
	if a.sidebarOpen {
		root.BorderWidth(1, 0, 0, 0).BorderColor(palette.sidebarBorder)
	}
	root.Children(func() {
		if len(a.openTabs) > 0 {
			a.tabRow(c)
		}
		switch {
		case a.fatalErr != "":
			a.centerNote(c, a.fatalErr, true)
			return
		case a.loading:
			a.centerNote(c, "Loading sessions", false)
			return
		case a.detailLoading:
			a.centerNote(c, "Loading conversation", false)
			return
		case a.detailErr != "":
			a.centerNote(c, a.detailErr, true)
			return
		case a.detail == nil:
			a.newSessionView(c)
			return
		}
		messages := a.detail.Messages
		if len(messages) == 0 {
			a.emptyState(c, false)
		} else {
			ui.Column(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
				ui.List(c, &a.messageList, len(a.transcriptRows), func(index int) {
					a.transcriptRowView(c, index)
				}).FillWidth().Grow(1)
				a.navigationRail(c)
				if !a.messageList.AtEnd() {
					ui.Row(c).Absolute().Left(0).Bottom(8).FillWidth().Justify(ui.Center).
						Children(func() {
							button := ui.ButtonBase(c).Label(tr("Scroll to bottom")).Size(34, 34).
								Radius(17).Center().Border(1, palette.borderStrong).
								Background(palette.composer)
							if button.Hovered() {
								button.Background(palette.raised)
							}
							if button.Pressed() {
								button.Background(palette.overlayStrong)
							}
							button.Children(func() {
								icon(c, "arrow-down", 17, palette.text)
							})
							if button.Clicked() {
								a.messageList.ScrollToEnd()
							}
						})
				}
			})
		}
		a.permissionCard(c)
		a.composer(c)
		a.workspaceFooter(c)
	})
}

// newSessionView is the empty state: onboarding without a project, or the
// "What should we build?" prompt over the composer for a chosen one.
func (a *app) newSessionView(c *ui.Context) {
	ui.Column(c).FillWidth().Grow(1).Children(func() {
		ui.Row(c).Grow(1).MinHeight(16)
		a.emptyState(c, true)
		a.permissionCard(c)
		a.composer(c)
		a.workspaceFooter(c)
		ui.Row(c).Grow(1).MinHeight(16)
	})
}

// emptyState mirrors render_empty_state: onboarding prompts, or the project
// question above the composer.
func (a *app) emptyState(c *ui.Context, centered bool) {
	project := a.selectedProject()
	padBottom := float32(52)
	if centered {
		padBottom = 24
	}
	if project == nil {
		ui.Column(c).FillWidth().Center().Padding(0, 32, padBottom, 32).Children(func() {
			icon(c, "sparkle", 24, palette.accent)
			ui.Text(c, tr("Open a project to begin")).FontSize(uiPx(20)).FontWeight(500).
				TextColor(palette.text).Margin(16, 0, 0, 0)
			ui.Text(c, tr("fintwind runs coding agents in folders you choose. Your code, sessions, and history stay on this computer")).
				FontSize(uiPx(12.5)).FixedLineHeight(uiPx(19)).TextColor(palette.textTertiary).
				MaxWidth(380).TextAlign(ui.Center).Margin(8, 0, 0, 0)
			ui.Column(c).AlignItems(ui.Center).Gap(8).Margin(20, 0, 0, 0).Children(func() {
				openButton := ui.ButtonBase(c).Label(tr("Open project folder")).Height(32).
					Padding(0, 14).Radius(16).Center().Background(palette.inverse)
				if openButton.Hovered() {
					openButton.Opacity(0.9)
				}
				if openButton.Pressed() {
					openButton.Opacity(0.8)
				}
				openButton.Children(func() {
					ui.Text(c, tr("Open project folder")).FontSize(uiPx(12.5)).FontWeight(600).
						TextColor(palette.onInverse)
				})
				if openButton.Clicked() {
					a.pickProjectFolder()
				}
				noProject := ui.ButtonBase(c).Label(tr("Don't work in a project")).Height(30).
					Padding(0, 12).Radius(15).Gap(6).AlignItems(ui.Center)
				if noProject.Hovered() {
					noProject.Background(palette.overlay)
				}
				if noProject.Pressed() {
					noProject.Background(palette.overlayStrong)
				}
				noProject.Children(func() {
					icon(c, "x", 11, palette.textTertiary)
					ui.Text(c, tr("Don't work in a project")).FontSize(uiPx(12)).
						TextColor(palette.textSecondary)
				})
				if noProject.Clicked() {
					a.createProjectless()
				}
			})
		})
		return
	}
	ui.Column(c).FillWidth().Center().Padding(0, 32, padBottom, 32).Children(func() {
		if project.Name == "No project" {
			ui.Text(c, tr("What should we build?")).FontSize(uiPx(20)).FontWeight(500).
				TextColor(palette.text)
			return
		}
		ui.Row(c).AlignItems(ui.Baseline).Gap(4).Children(func() {
			ui.Text(c, tr("What should we build in")).FontSize(uiPx(20)).FontWeight(500).
				TextColor(palette.text)
			a.emptyProjectSelector(c, project)
			ui.Text(c, tr("?")).FontSize(uiPx(20)).FontWeight(500).TextColor(palette.text)
		})
	})
}

// emptyProjectSelector is the inline project name in the new-task headline:
// 20 px medium text over a 1 px underline, opening the project menu.
func (a *app) emptyProjectSelector(c *ui.Context, project *model.Project) {
	selector := ui.ButtonBase(c).Label(project.Name).Children(func() {
		ui.Column(c).Children(func() {
			ui.Text(c, project.Name).FontSize(uiPx(20)).FontWeight(500).TextColor(palette.text)
			underline := palette.textTertiary
			if a.emptyProjectMenuOpen {
				underline = palette.textSecondary
			}
			ui.Row(c).FillWidth().Height(1).Background(underline)
		})
	})
	if selector.Clicked() {
		a.emptyProjectMenuOpen = !a.emptyProjectMenuOpen
	}
	ui.PopoverBase(c, selector, &a.emptyProjectMenuOpen, func(panel ui.Element) {
		panel.Background(palette.raised).Border(1, palette.borderStrong).Radius(9).
			Padding(4).Width(210).
			Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.28)).
			Shadow(0, 12, 28, 0, ui.RGBA(0, 0, 0, 0.22)).
			Children(func() {
				for index := range a.projects {
					candidate := &a.projects[index]
					if candidate.Name == "No project" {
						continue
					}
					a.projectMenuItem(c, candidate, candidate.ID == project.ID)
				}
				ui.Row(c).FillWidth().Height(1).Margin(4, 0, 4, 0).
					Background(palette.border)
				newProject := ui.ButtonBase(c).Label(tr("New project")).FillWidth().Height(30).
					Padding(0, 8).Radius(6).Gap(8).AlignItems(ui.Center)
				if newProject.Hovered() {
					newProject.Background(palette.overlay)
				}
				newProject.Children(func() {
					icon(c, "folder-plus", 14, palette.textSecondary)
					ui.Text(c, tr("New project")).FontSize(uiPx(12.5)).TextColor(palette.text)
				})
				if newProject.Clicked() {
					a.emptyProjectMenuOpen = false
					a.pickProjectFolder()
				}
				noProject := ui.ButtonBase(c).Label(tr("Don't work in a project")).FillWidth().
					Height(30).Padding(0, 8).Radius(6).Gap(8).AlignItems(ui.Center)
				if noProject.Hovered() {
					noProject.Background(palette.overlay)
				}
				noProject.Children(func() {
					icon(c, "x", 14, palette.textSecondary)
					ui.Text(c, tr("Don't work in a project")).FontSize(uiPx(12.5)).TextColor(palette.text)
				})
				if noProject.Clicked() {
					a.emptyProjectMenuOpen = false
					a.createProjectless()
				}
			})
	})
}

func (a *app) centerNote(c *ui.Context, text string, isError bool) {
	ui.Column(c).FillWidth().Grow(1).Center().Padding(0, 32).Children(func() {
		ui.Text(c, text).FontSize(uiPx(13)).
			TextColor(choice(isError, palette.danger, palette.textMuted))
	})
}

func (a *app) transcriptRowView(c *ui.Context, index int) {
	if index < 0 || index >= len(a.transcriptRows) {
		return
	}
	row := a.transcriptRows[index]
	count := len(a.transcriptRows)
	top := float32(8)
	bottom := float32(8)
	if index == 0 {
		top = 22
	}
	if row.Kind == transcriptMessage && a.detail != nil &&
		messageStartsFollowupTurn(a.detail.Messages, row.MessageIndex) {
		top = 48
	}
	if index+1 == count {
		bottom = 22
	}
	ui.Row(c).FillWidth().Justify(ui.Center).Padding(top, 20, bottom, 20).Children(func() {
		ui.Column(c).FillWidth().MaxWidth(transcriptWidth).MinWidth(0).Children(func() {
			switch row.Kind {
			case transcriptMessage:
				if row.MessageIndex >= 0 && row.MessageIndex < len(a.detail.Messages) {
					a.messageView(c, row.MessageIndex)
				}
			case transcriptBlock:
				if row.BlockIndex < 0 || row.BlockIndex >= len(a.detail.TranscriptBlocks) {
					return
				}
				liveTurn := false
				if active := a.detail.ActiveTurnID(); active != "" {
					block := a.detail.TranscriptBlocks[row.BlockIndex]
					if block.TurnID != nil && *block.TurnID == active {
						liveTurn = true
					}
				}
				latest := row.BlockIndex+1 == len(a.detail.TranscriptBlocks)
				a.activityCluster(c, row.BlockIndex, liveTurn, latest)
			case transcriptFold:
				a.turnFoldRow(c, row.TurnID)
			case transcriptWorking:
				a.workingRow(c)
			case transcriptChangedFiles:
				a.changedFilesCard(c, row.TurnID, false)
			case transcriptCompacting:
				a.compactingRow(c)
			}
		})
	})
}

func (a *app) turnFoldRow(c *ui.Context, turnID string) {
	expanded := a.expandedTurns[turnID]
	label := tr("Worked")
	for index := range a.detail.Turns {
		turn := &a.detail.Turns[index]
		if turn.ID != turnID {
			continue
		}
		end := uint64(time.Now().Unix())
		if turn.CompletedAt != nil {
			end = *turn.CompletedAt
		}
		seconds := end - turn.StartedAt
		if seconds < 1 {
			seconds = 1
		}
		duration := formatWorkedDuration(seconds)
		if turn.Status == "interrupted" {
			label = tr("You stopped after ") + duration
		} else {
			label = tr("Worked for ") + duration
		}
		break
	}
	ui.Row(c).FillWidth().Height(28).Gap(10).AlignItems(ui.Center).Children(func() {
		ui.Row(c).Height(1).Grow(1).Background(palette.border)
		button := ui.ButtonBase(c).Label(label).Height(28).Padding(0, 3).Shrink(0).
			Gap(5).AlignItems(ui.Center)
		button.Children(func() {
			color := palette.textTertiary
			if button.Hovered() || button.Pressed() {
				color = palette.text
			}
			ui.Text(c, label).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(17)).FontWeight(500).
				TextColor(color)
			chevron := "chevron-right"
			if expanded {
				chevron = "chevron-down"
			}
			icon(c, chevron, 11, palette.textTertiary)
		})
		if button.Clicked() {
			a.expandedTurns[turnID] = !expanded
			a.rebuildTranscript()
		}
		ui.Row(c).Height(1).Grow(1).Background(palette.border)
	})
}

func (a *app) workingRow(c *ui.Context) {
	elapsed := uint64(0)
	if a.detail != nil && len(a.detail.Turns) > 0 {
		last := &a.detail.Turns[len(a.detail.Turns)-1]
		if last.Status == "running" {
			now := uint64(time.Now().Unix())
			if now > last.StartedAt {
				elapsed = now - last.StartedAt
			}
		}
	}
	ui.Row(c).Height(22).Gap(8).AlignItems(ui.Center).Children(func() {
		ui.Row(c).Gap(3).AlignItems(ui.Center).Children(func() {
			for index := 0; index < 3; index++ {
				ui.Row(c).Size(4, 4).Radius(2).Background(palette.textTertiary)
			}
		})
		ui.Text(c, tr("Working for ")+formatWorkingElapsed(elapsed)).FontSize(uiPx(11.5)).
			FixedLineHeight(uiPx(16)).FontWeight(500).TextColor(palette.textTertiary)
	})
}

// compactingRow closes the transcript while the provider compacts context, a
// state that runs outside any turn.
func (a *app) compactingRow(c *ui.Context) {
	ui.Row(c).Height(22).Gap(8).AlignItems(ui.Center).Children(func() {
		ui.Row(c).Gap(3).AlignItems(ui.Center).Children(func() {
			for index := 0; index < 3; index++ {
				ui.Row(c).Size(4, 4).Radius(2).Background(palette.textTertiary)
			}
		})
		ui.Text(c, tr("Compacting context…")).FontSize(uiPx(11.5)).FixedLineHeight(uiPx(16)).
			FontWeight(500).TextColor(palette.textTertiary)
	})
}

func formatWorkingElapsed(seconds uint64) string {
	switch {
	case seconds < 60:
		return fmt.Sprintf("%ds", seconds)
	case seconds < 3600:
		if seconds%60 == 0 {
			return fmt.Sprintf("%dm", seconds/60)
		}
		return fmt.Sprintf("%dm %ds", seconds/60, seconds%60)
	default:
		if (seconds%3600)/60 == 0 {
			return fmt.Sprintf("%dh", seconds/3600)
		}
		return fmt.Sprintf("%dh %dm", seconds/3600, (seconds%3600)/60)
	}
}

func (a *app) messageView(c *ui.Context, messageIndex int) {
	if messageIndex < 0 || messageIndex >= len(a.detail.Messages) {
		return
	}
	m := &a.detail.Messages[messageIndex]
	text := m.Text()
	switch m.Role {
	case "user":
		if a.editMessage != nil && a.editMessage.MessageID == m.ID {
			column := ui.Column(c).FillWidth().AlignItems(ui.End).Gap(3)
			column.Children(func() {
				if len(m.Attachments) > 0 {
					ui.Row(c).MaxWidth(540).Wrap().Justify(ui.End).Gap(8).Children(func() {
						for _, attachment := range m.Attachments {
							a.attachmentChip(c, attachment)
						}
					})
				}
				a.messageEditCard(c, m)
			})
			return
		}
		collapsible := userMessageCollapsible(text)
		collapsed := collapsible && !a.expandedUserMessages[m.ID]
		column := ui.Column(c).FillWidth().AlignItems(ui.End).Gap(3)
		column.Selectable()
		column.Children(func() {
			if len(m.Attachments) > 0 {
				ui.Row(c).MaxWidth(540).Wrap().Justify(ui.End).Gap(8).Children(func() {
					for _, attachment := range m.Attachments {
						a.attachmentChip(c, attachment)
					}
				})
			}
			// The source renders the bubble only for non-empty text, so an
			// attachment-only message shows its tiles with no empty box.
			if len(m.Attachments) > 0 {
				ui.Row(c).MaxWidth(540).Wrap().Justify(ui.End).Gap(8).Children(func() {
					for _, attachment := range m.Attachments {
						a.attachmentChip(c, attachment)
					}
				})
			}
			// The source renders the bubble only for non-empty text, so an
			// attachment-only message shows its tiles with no empty box.
			if text != "" {
				bubble := ui.Column(c).MaxWidth(540).MinWidth(0).Background(palette.raised).
					Radius(12).Padding(8, 12, 8, 12).Children(func() {
					if collapsed {
						ui.Column(c).Height(120).Clip().Children(func() {
							ui.Row(c).MaxHeight(120).Children(func() {
								a.renderMarkdown(c, "user:"+m.ID, text, &a.mdUser, false)
							})
							ui.Row(c).Absolute().Left(0).Right(0).Bottom(0).Height(32).
								LinearGradient(ui.LinearGradient{
									From:  ui.RGBA(palette.raised.R, palette.raised.G, palette.raised.B, 0),
									To:    palette.raised,
									Angle: 180,
								})
						})
					} else {
						a.renderMarkdown(c, "user:"+m.ID, text, &a.mdUser, false)
					}
					if collapsible {
						toggle := ui.ButtonBase(c).Label(tr("Show more")).Margin(2, 0, 0, 0).
							Height(24).Gap(3).AlignItems(ui.Center).Shrink(0)
						toggle.Children(func() {
							chevron := "chevron-down"
							if !collapsed {
								chevron = "chevron-up"
							}
							icon(c, chevron, 12, palette.textTertiary)
							color := palette.textTertiary
							if toggle.Hovered() || toggle.Pressed() {
								color = palette.textSecondary
							}
							label := tr("Show more")
							if !collapsed {
								label = tr("Show less")
							}
							ui.Text(c, label).FontSize(uiPx(11.5)).FontWeight(500).TextColor(color)
						})
						if toggle.Clicked() {
							a.expandedUserMessages[m.ID] = collapsed
						}
					}
				})
				_ = bubble
			}
			_ = a.messageCopyMenu(column, m.ID, "user", text)
			footer := a.messageFooter(c, m, messageIndex, text, true)
			if !column.Hovered() {
				footer.Invisible()
			}
		})
	case "assistant":
		column := ui.Column(c).FillWidth().MinWidth(0).PaddingY(4).Gap(3)
		column.Selectable()
		column.Children(func() {
			if text != "" {
				a.renderMarkdown(c, "msg:"+m.ID, text, &a.md, m.Streaming)
			}
			if m.TurnID != nil && changedFilesInlineMessageIndex(a.detail, *m.TurnID) == messageIndex {
				a.changedFilesCard(c, *m.TurnID, true)
			}
			if copyContent, at, ok := a.assistantFooter(messageIndex); ok || a.assistantTurnStats(messageIndex) != "" {
				row := ui.Row(c).FillWidth().MinWidth(0).Gap(8).AlignItems(ui.Center)
				row.Children(func() {
					if stats := a.assistantTurnStats(messageIndex); stats != "" {
						ui.Row(c).Grow(1).MinWidth(0).Children(func() {
							ui.Text(c, stats).FontSize(uiPx(11.5)).FixedLineHeight(uiPx(16)).
								TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
						})
					}
					if ok {
						footer := a.assistantFooterRow(c, m, messageIndex, copyContent, at)
						footer.Margin(0, 0, 0, -7)
						if !column.Hovered() {
							footer.Invisible()
						}
					}
				})
			}
		})
		// The row menu copies what the footer would copy, matching the
		// source's menu_copy_content.
		menuCopy := text
		if content, _, ok := a.assistantFooter(messageIndex); ok {
			menuCopy = content
		}
		_ = a.messageCopyMenu(column, m.ID, "assistant", menuCopy)
	case "system":
		a.systemMessage(c, text)
	case "compaction":
		a.compactionMessage(c, m.ID, text)
	default:
		if text != "" {
			ui.Text(c, text).FontSize(uiPx(13.5)).FixedLineHeight(uiPx(21)).TextColor(palette.text)
		}
	}
}

// userMessageCollapsible folds a bubble once its text would run past roughly
// nine wrapped lines, estimated from character widths as Fintwind does.
func userMessageCollapsible(content string) bool {
	const unitsPerLine = 72
	const lineLimit = 9
	if len(content) < unitsPerLine*lineLimit {
		return false
	}
	lines := 0
	for _, line := range strings.Split(content, "\n") {
		units := 0
		for _, r := range line {
			if r < 128 {
				units++
			} else {
				units += 2
			}
		}
		wrapped := (units + unitsPerLine - 1) / unitsPerLine
		if wrapped < 1 {
			wrapped = 1
		}
		lines += wrapped
		if lines > lineLimit {
			return true
		}
	}
	return false
}

// assistantFooter resolves the settled answer's copy content and timestamp.
func (a *app) assistantFooter(messageIndex int) (string, uint64, bool) {
	session := a.detail
	if session == nil || messageIndex < 0 || messageIndex >= len(session.Messages) {
		return "", 0, false
	}
	message := &session.Messages[messageIndex]
	if message.Role != "assistant" || message.Streaming {
		return "", 0, false
	}
	if message.TurnID == nil {
		return message.Content, message.CreatedAt, true
	}
	turnID := *message.TurnID
	turn := a.turnByID(turnID)
	if turn != nil && turn.Status == "running" {
		return "", 0, false
	}
	last := -1
	for index := range session.Messages {
		candidate := &session.Messages[index]
		if candidate.Role == "assistant" && candidate.TurnID != nil && *candidate.TurnID == turnID {
			last = index
		}
	}
	if last != messageIndex {
		return "", 0, false
	}
	rows := turnRows(session, turnID)
	start := turnAnswerStart(session, rows)
	parts := make([]string, 0, 2)
	for _, row := range rows[start:] {
		if row.Kind != transcriptMessage {
			continue
		}
		part := &session.Messages[row.MessageIndex]
		if strings.TrimSpace(part.Content) != "" {
			parts = append(parts, part.Content)
		}
	}
	at := message.CreatedAt
	if turn != nil && turn.CompletedAt != nil {
		at = *turn.CompletedAt
	}
	return strings.Join(parts, "\n\n"), at, true
}

// assistantTurnStats builds the settled turn's stats line:
// "Build · model · 25.9s · 64.5 tok/s". Segments drop out when missing.
func (a *app) assistantTurnStats(messageIndex int) string {
	session := a.detail
	if session == nil || messageIndex < 0 || messageIndex >= len(session.Messages) {
		return ""
	}
	message := &session.Messages[messageIndex]
	if _, _, ok := a.assistantFooter(messageIndex); !ok {
		return ""
	}
	if message.TurnID == nil {
		return ""
	}
	turn := a.turnByID(*message.TurnID)
	if turn == nil {
		return ""
	}
	stats := turn.Stats
	if stats == nil && turn.CompletedAt == nil {
		return ""
	}
	segments := make([]string, 0, 4)
	if stats != nil && stats.Agent != nil && *stats.Agent != "" {
		segments = append(segments, titlecaseAgent(*stats.Agent))
	} else {
		label := "Build"
		if session.InteractionMode == "plan" {
			label = "Plan"
		}
		segments = append(segments, label)
	}
	if stats != nil && stats.Model != nil && *stats.Model != "" {
		segments = append(segments, modelDisplayName(*stats.Model))
	}
	if turn.CompletedAt != nil {
		ms := (*turn.CompletedAt - turn.StartedAt) * 1000
		segments = append(segments, formatTurnStatsDuration(ms))
	}
	if stats != nil && stats.StreamVerified && stats.OutputTokens > 0 && stats.StreamMs > 0 {
		speed := float64(stats.OutputTokens) / (float64(stats.StreamMs) / 1000.0)
		segments = append(segments, fmt.Sprintf("%.1f tok/s", speed))
	}
	return strings.Join(segments, " · ")
}

func modelDisplayName(key string) string {
	if index := strings.LastIndex(key, "/"); index >= 0 {
		return key[index+1:]
	}
	return key
}

func formatTurnStatsDuration(ms uint64) string {
	switch {
	case ms < 1_000:
		return fmt.Sprintf("%dms", ms)
	case ms < 60_000:
		return fmt.Sprintf("%.1fs", float64(ms)/1000.0)
	case ms < 3_600_000:
		return fmt.Sprintf("%dm %ds", ms/60_000, (ms%60_000)/1_000)
	case ms < 86_400_000:
		return fmt.Sprintf("%dh %dm", ms/3_600_000, (ms%3_600_000)/60_000)
	default:
		return fmt.Sprintf("%dd %dh", ms/86_400_000, (ms%86_400_000)/3_600_000)
	}
}

func titlecaseAgent(agent string) string {
	var out strings.Builder
	atWordStart := true
	for _, r := range agent {
		isWord := (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') ||
			(r >= '0' && r <= '9') || r == '_'
		if isWord {
			if atWordStart {
				out.WriteString(strings.ToUpper(string(r)))
			} else {
				out.WriteRune(r)
			}
			atWordStart = false
		} else {
			out.WriteRune(r)
			atWordStart = true
		}
	}
	return out.String()
}

func (a *app) turnByID(turnID string) *model.AgentTurn {
	for index := range a.detail.Turns {
		if a.detail.Turns[index].ID == turnID {
			return &a.detail.Turns[index]
		}
	}
	return nil
}

func (a *app) messageFooter(c *ui.Context, m *model.Message, messageIndex int, copyContent string, alignRight bool) ui.Element {
	footer := ui.Row(c).Height(27).Gap(1).AlignItems(ui.Center)
	if alignRight {
		footer.FillWidth().Justify(ui.End)
	}
	footer.Children(func() {
		if alignRight {
			a.messageTime(c, m.CreatedAt)
		}
		copyButton := ui.ButtonBase(c).Label(tr("Copy message")).Size(30, 30).Radius(8).Center()
		if copyButton.Hovered() {
			copyButton.Background(palette.overlayStrong)
		}
		copied := a.copiedMessages[m.ID] != 0
		copyButton.Children(func() {
			iconName := "copy"
			if copied {
				iconName = "check"
			}
			icon(c, iconName, 15, palette.textMuted)
		})
		if copyButton.Clicked() {
			_ = mygo.Clipboard.Write(transfer.TextData(copyContent))
			a.showCopied(m.ID)
		}
		if !alignRight {
			a.messageTime(c, m.CreatedAt)
		}
		if alignRight {
			turnCount, ready, tip := a.editRewindState(messageIndex)
			_ = turnCount
			if tip != "" {
				rewind := ui.ButtonBase(c).Label(tip).Tooltip(tip).Size(27, 27).
					Radius(8).Center().Shrink(0)
				if ready {
					if rewind.Hovered() {
						rewind.Background(palette.overlayStrong)
					}
				} else {
					rewind.Opacity(0.45)
				}
				rewind.Children(func() {
					icon(c, "rewind", 14, palette.textMuted)
				})
				if ready && rewind.Clicked() {
					a.beginMessageEdit(messageIndex)
				}
			}
		}
	})
	return footer
}

func (a *app) assistantFooterRow(c *ui.Context, m *model.Message, messageIndex int, copyContent string, at uint64) ui.Element {
	footer := ui.Row(c).Height(27).Gap(1).AlignItems(ui.Center).Shrink(0)
	footer.Children(func() {
		copyButton := ui.ButtonBase(c).Label(tr("Copy message")).Size(30, 30).Radius(8).Center()
		if copyButton.Hovered() {
			copyButton.Background(palette.overlayStrong)
		}
		copied := a.copiedMessages[m.ID] != 0
		copyButton.Children(func() {
			iconName := "copy"
			if copied {
				iconName = "check"
			}
			icon(c, iconName, 15, palette.textMuted)
		})
		if copyButton.Clicked() {
			_ = mygo.Clipboard.Write(transfer.TextData(copyContent))
			a.showCopied(m.ID)
		}
		// The fork button follows the copy button, mirroring
		// render_message_footer: 30x30, radius 8, 15 icon in text_muted,
		// hover overlay only while live, dimmed while another fork runs,
		// and a spinning loader while this response's fork prepares.
		if action := a.forkActionForMessage(messageIndex); action != nil {
			tip := "Fork task"
			if !action.enabled {
				tip = "Forking task…"
			}
			fork := ui.ButtonBase(c).Label(tip).Tooltip(tip).Size(30, 30).Radius(8).Center()
			if action.enabled {
				if fork.Hovered() {
					fork.Background(palette.overlayStrong)
				}
			} else if !action.preparing {
				fork.Opacity(0.45)
			}
			fork.Children(func() {
				if action.preparing {
					c.AnimationFrame()
					icon(c, "loader-circle", 15, palette.textMuted).
						Rotate(float32(c.Now().UnixMilli()%1000) * 0.36)
				} else {
					icon(c, "fork", 15, palette.textMuted)
				}
			})
			if action.enabled && fork.Clicked() {
				a.forkSessionFromResponse(action.sessionID, action.messageID, action.turnCount)
			}
		}
		a.messageTime(c, at)
	})
	return footer
}

func (a *app) showCopied(messageID string) {
	if a.copiedMessages == nil {
		a.copiedMessages = map[string]uint64{}
	}
	a.copiedGeneration++
	generation := a.copiedGeneration
	a.copiedMessages[messageID] = generation
	time.AfterFunc(2*time.Second, func() {
		a.post(func() {
			if a.copiedMessages[messageID] == generation {
				delete(a.copiedMessages, messageID)
			}
		})
	})
}

func (a *app) messageTime(c *ui.Context, at uint64) {
	ui.Text(c, formatMessageTime(at)).FontSize(uiPx(11.5)).FixedLineHeight(uiPx(14)).
		TextColor(palette.textMuted).Height(27).Padding(0, 4).Shrink(0)
}

func (a *app) systemMessage(c *ui.Context, text string) {
	ui.Row(c).FillWidth().Justify(ui.Center).Children(func() {
		card := ui.Row(c).MaxWidth(560).MinWidth(0).Padding(6, 10, 6, 10).Radius(8).
			Background(palette.dangerSoft).Border(1, palette.danger).Gap(7).
			AlignItems(ui.Start)
		card.Children(func() {
			ui.Row(c).Shrink(0).Padding(1, 0, 0, 0).Children(func() {
				icon(c, "alert", 13, palette.danger)
			})
			ui.Row(c).Grow(1).MinWidth(0).Children(func() {
				ui.Text(c, text).FontSize(uiPx(11.5)).FixedLineHeight(uiPx(16)).
					TextColor(palette.text).Wrap()
			})
		})
	})
}

func (a *app) compactionMessage(c *ui.Context, messageID, text string) {
	expanded := a.expandedCompaction[messageID]
	preview := ""
	for _, line := range strings.Split(text, "\n") {
		line = strings.TrimSpace(line)
		if line != "" {
			preview = strings.Trim(strings.TrimSpace(strings.TrimLeft(line, "#*")), "*")
			preview = strings.TrimSpace(preview)
			break
		}
	}
	card := ui.Column(c).FillWidth().MinWidth(0).Radius(9).
		Border(1, palette.borderStrong).Background(hex(0x2A2A2A))
	card.Selectable()
	card.Children(func() {
		row := ui.ButtonBase(c).Label(tr("Compaction")).FillWidth().Height(30).
			Padding(0, 10).Gap(6).AlignItems(ui.Center)
		row.Children(func() {
			color := palette.textSecondary
			if row.Hovered() {
				color = palette.text
			} else if row.Pressed() {
				color = palette.textMuted
			}
			ui.Text(c, tr("Compaction")).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(17)).
				FontWeight(500).TextColor(color).Shrink(0)
			if !expanded && preview != "" {
				ui.Row(c).Grow(1).MinWidth(0).Children(func() {
					ui.Text(c, preview).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(17)).
						TextColor(palette.textTertiary).SingleLine().Ellipsis("...")
				})
			} else {
				ui.Row(c).Grow(1)
			}
			chevron := "chevron-right"
			if expanded {
				chevron = "chevron-down"
			}
			icon(c, chevron, 11, palette.textTertiary)
		})
		if row.Clicked() {
			if a.expandedCompaction == nil {
				a.expandedCompaction = map[string]bool{}
			}
			a.expandedCompaction[messageID] = !expanded
		}
		if expanded {
			ui.Column(c).MinWidth(0).Padding(0, 10, 8, 10).Children(func() {
				a.renderMarkdown(c, "compaction:"+messageID, text, &a.md, false)
			})
		}
	})
}

// changedFilesCard is the turn's file summary: a 58px head with the totals and
// a Review button, then one 31px row per changed file (three before the
// "show more" row).
func (a *app) changedFilesCard(c *ui.Context, turnID string, inline bool) {
	if a.detail == nil {
		return
	}
	turn := a.detail.TurnByID(turnID)
	if turn == nil || !turnHasChangedFiles(turn) {
		return
	}
	files := turn.Checkpoint.Files
	expanded := a.expandedChangedFiles[turnID]
	limit := changedFilesPreviewLimit
	if expanded {
		limit = changedFilesExpandedLimit
	}
	visible := len(files)
	if visible > limit {
		visible = limit
	}
	additions, deletions := uint64(0), uint64(0)
	for _, file := range files {
		additions += file.Additions
		deletions += file.Deletions
	}
	card := ui.Column(c).FillWidth().MinWidth(0).Radius(cardRadius).
		Background(palette.raised).Border(1, palette.cardBorder).Clip()
	card = cardShadow(card)
	card.Children(func() {
		head := ui.Row(c).FillWidth().MinHeight(58).Padding(9, 12).Gap(10).
			AlignItems(ui.Center)
		head.Children(func() {
			ui.Row(c).Size(36, 36).Shrink(0).Radius(3).Background(palette.overlayStrong).
				Center().Children(func() {
				icon(c, "file-diff", 16, palette.textTertiary)
			})
			ui.Column(c).Grow(1).MinWidth(0).Gap(2).Children(func() {
				title := fmt.Sprintf(tr("Changed %d files"), len(files))
				if len(files) == 1 {
					title = tr("Changed 1 file")
				}
				ui.Text(c, title).FontSize(uiPx(12.5)).FontWeight(500).TextColor(palette.text).
					SingleLine().Ellipsis("...")
				ui.Row(c).Gap(6).AlignItems(ui.Center).Children(func() {
					ui.Text(c, fmt.Sprintf(tr("+%d"), additions)).FontSize(uiPx(11)).
						FixedLineHeight(uiPx(14)).TextColor(palette.successText)
					ui.Text(c, fmt.Sprintf("-%d", deletions)).FontSize(uiPx(11)).
						FixedLineHeight(uiPx(14)).TextColor(palette.dangerText)
				})
			})
			review := ui.ButtonBase(c).Label(tr("Review")).Height(32).Padding(0, 11).
				Radius(8).Border(1, palette.borderStrong).Gap(6).AlignItems(ui.Center)
			if review.Pressed() {
				review.Background(palette.overlayStrong)
			} else if review.Hovered() {
				review.Background(palette.overlay)
			}
			review.Children(func() {
				icon(c, "file-diff", 13, palette.textTertiary)
				ui.Text(c, tr("Review")).FontSize(uiPx(12.5)).FontWeight(500).
					TextColor(palette.textSecondary)
			})
			if review.Clicked() {
				a.openTurnReview(turnID)
			}
		})
		rows := ui.Column(c).FillWidth().MinWidth(0).
			BorderWidth(1, 0, 0, 0).BorderColor(palette.border)
		rows.Children(func() {
			for index := 0; index < visible; index++ {
				file := files[index]
				ui.Row(c).FillWidth().Height(31).Padding(0, 12).Gap(8).
					AlignItems(ui.Center).Children(func() {
					ui.Row(c).Grow(1).MinWidth(0).Children(func() {
						ui.Text(c, file.Path).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).
							SingleLine().Ellipsis("...")
					})
					ui.Text(c, fmt.Sprintf(tr("+%d"), file.Additions)).FontSize(uiPx(10.5)).
						TextColor(palette.successText).Shrink(0)
					ui.Text(c, fmt.Sprintf("-%d", file.Deletions)).FontSize(uiPx(10.5)).
						TextColor(palette.dangerText).Shrink(0)
				})
			}
		})
		if len(files) > changedFilesPreviewLimit {
			label := fmt.Sprintf(tr("Show %d more files"), len(files)-changedFilesPreviewLimit)
			if expanded {
				label = tr("Show fewer files")
			}
			toggle := ui.ButtonBase(c).Label(label).FillWidth().Height(36).
				Padding(0, 12).Gap(6).AlignItems(ui.Center).
				BorderWidth(1, 0, 0, 0).BorderColor(palette.border)
			if toggle.Pressed() {
				toggle.Background(palette.overlayStrong)
			} else if toggle.Hovered() {
				toggle.Background(palette.overlay)
			}
			toggle.Children(func() {
				ui.Text(c, label).FontSize(uiPx(12.5)).FontWeight(500).
					TextColor(palette.textSecondary).Shrink(0)
				if expanded && len(files) > changedFilesExpandedLimit {
					ui.Text(c, fmt.Sprintf(tr("Showing first %d of %d"), changedFilesExpandedLimit, len(files))).
						FontSize(uiPx(12.5)).TextColor(palette.textMuted).
						SingleLine().Ellipsis("...")
				}
				ui.Row(c).Grow(1)
				chevron := "chevron-right"
				if expanded {
					chevron = "chevron-down"
				}
				icon(c, chevron, 12, palette.textTertiary)
			})
			if toggle.Clicked() {
				if a.expandedChangedFiles == nil {
					a.expandedChangedFiles = map[string]bool{}
				}
				a.expandedChangedFiles[turnID] = !expanded
				a.rebuildTranscript()
			}
		}
	})
}

// attachmentChip is one sent-file tile: 96x80, 9 radius, border, inset fill.
func (a *app) attachmentChip(c *ui.Context, attachment model.Attachment) {
	tile := ui.Column(c).Size(96, 80).Radius(9).Clip().Border(1, palette.border).
		Background(palette.inset)
	tile.Children(func() {
		if attachment.IsImage {
			ui.Row(c).FillWidth().Grow(1).Center().Children(func() {
				icon(c, "modality-image", 18, palette.textSecondary)
			})
			return
		}
		ui.Column(c).FillWidth().Grow(1).Padding(0, 7, 0, 7).Center().
			Gap(7).Children(func() {
			icon(c, "file", 18, palette.textSecondary)
			ui.Text(c, attachment.Name).FontSize(uiPx(9.5)).TextColor(palette.textSecondary).
				TextAlign(ui.Center).SingleLine().Ellipsis("...")
		})
	})
}

func (a *app) workspaceFooter(c *ui.Context) {
	project := a.selectedProject()
	if project == nil {
		return
	}
	canConfigure := a.detail == nil ||
		(!a.detail.Busy() && len(a.detail.Turns) == 0 && len(a.detail.Messages) == 0)
	ui.Column(c).FillWidth().Shrink(0).Padding(4, 20, 8, 20).Children(func() {
		ui.Row(c).FillWidth().MaxWidth(transcriptWidth).Margin(0, ui.Auto).Height(32).
			Padding(0, 10).Gap(2).AlignItems(ui.Center).Children(func() {
			a.projectFooterChip(c, project, canConfigure)
			a.footerChip(c, "laptop", "Local", true)
			if a.branchLabel != "" {
				busy := a.detail != nil && a.detail.Busy()
				a.footerChip(c, "git-branch", a.branchLabel, busy)
			}
			ui.Row(c).Grow(1)
			a.usageMeter(c)
		})
	})
}

// projectFooterChip is the footer's workspace selector. It opens the project
// menu only while the draft can still change workspace, matching the source's
// disabled state once a task has started.
func (a *app) projectFooterChip(c *ui.Context, project *model.Project, enabled bool) {
	label := project.Name
	if project.Name == "No project" {
		label = tr("Choose project")
	}
	chip := ui.ButtonBase(c).Label(label).Height(28).Padding(0, 9).Radius(7).
		Gap(7).AlignItems(ui.Center)
	if enabled {
		if a.projectMenuOpen {
			chip.Background(palette.overlay)
		}
		if chip.Hovered() {
			chip.Background(palette.overlay)
		}
		if chip.Pressed() {
			chip.Background(palette.overlayStrong)
		}
	} else {
		chip.Opacity(0.7)
	}
	chip.Children(func() {
		icon(c, "folder", 12, palette.textTertiary)
		ui.Row(c).MinWidth(0).MaxWidth(190).Children(func() {
			ui.Text(c, label).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(16)).
				TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
		})
	})
	if enabled && chip.Clicked() {
		a.projectMenuOpen = !a.projectMenuOpen
	}
	if !enabled {
		return
	}
	ui.PopoverBase(c, chip, &a.projectMenuOpen, func(panel ui.Element) {
		panel.Background(palette.raised).Border(1, palette.borderStrong).Radius(9).
			Padding(4).Width(200).
			Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.28)).
			Shadow(0, 12, 28, 0, ui.RGBA(0, 0, 0, 0.22)).
			Children(func() {
				for index := range a.projects {
					candidate := &a.projects[index]
					if candidate.EnvironmentID != "" {
						continue
					}
					a.projectMenuItem(c, candidate, candidate.ID == project.ID)
				}
				ui.Row(c).FillWidth().Height(1).Margin(4, 0, 4, 0).
					Background(palette.border)
				newProject := ui.ButtonBase(c).Label(tr("New project")).FillWidth().Height(30).
					Padding(0, 8).Radius(6).Gap(8).AlignItems(ui.Center)
				if newProject.Hovered() {
					newProject.Background(palette.overlay)
				}
				newProject.Children(func() {
					icon(c, "folder-plus", 14, palette.textSecondary)
					ui.Text(c, tr("New project")).FontSize(uiPx(12.5)).TextColor(palette.text)
				})
				if newProject.Clicked() {
					a.projectMenuOpen = false
					a.pickProjectFolder()
				}
				noProject := ui.ButtonBase(c).Label(tr("No project")).FillWidth().Height(30).
					Padding(0, 8).Radius(6).Gap(8).AlignItems(ui.Center)
				if noProject.Hovered() {
					noProject.Background(palette.overlay)
				}
				noProject.Children(func() {
					icon(c, "x", 14, palette.textSecondary)
					ui.Text(c, tr("No project")).FontSize(uiPx(12.5)).TextColor(palette.text)
				})
				if noProject.Clicked() {
					a.projectMenuOpen = false
					a.createProjectless()
				}
			})
	})
}

func (a *app) projectMenuItem(c *ui.Context, project *model.Project, selected bool) {
	item := ui.ButtonBase(c).Label(project.Name).FillWidth().Height(30).
		Padding(0, 8).Radius(6).Gap(8).AlignItems(ui.Center)
	if item.Hovered() {
		item.Background(palette.overlay)
	}
	item.Children(func() {
		icon(c, "folder", 14, palette.textTertiary)
		ui.Row(c).Grow(1).MinWidth(0).Children(func() {
			ui.Text(c, project.Name).FontSize(uiPx(12.5)).TextColor(palette.text).
				SingleLine().Ellipsis("...")
		})
		if selected {
			icon(c, "check", 13, palette.text)
		}
	})
	if item.Clicked() {
		a.projectMenuOpen = false
		a.lastProject = project.ID
		a.newTab()
	}
}

// footerChip is the footer's MenuChip: 28 high, 9 insets, 7 gap.
func (a *app) footerChip(c *ui.Context, iconName, label string, disabled bool) {
	limit := float32(210)
	if iconName == "laptop" {
		limit = 180
	}
	chip := ui.ButtonBase(c).Label(label).Height(28).Padding(0, 9).Radius(7).
		Gap(7).AlignItems(ui.Center)
	if !disabled {
		if chip.Hovered() {
			chip.Background(palette.overlay)
		}
		if chip.Pressed() {
			chip.Background(palette.overlayStrong)
		}
	} else {
		chip.Opacity(0.7)
	}
	chip.Children(func() {
		icon(c, iconName, 12, palette.textTertiary)
		ui.Row(c).MinWidth(0).MaxWidth(limit).Children(func() {
			ui.Text(c, label).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(16)).
				TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
		})
	})
}

func shortPath(value string) string {
	if len(value) <= 68 {
		return value
	}
	parts := strings.Split(strings.ReplaceAll(value, "\\", "/"), "/")
	if len(parts) <= 3 {
		return value
	}
	return ".../" + strings.Join(parts[len(parts)-3:], "/")
}

func formatMessageTime(unix uint64) string {
	if unix == 0 {
		return ""
	}
	return time.Unix(int64(unix), 0).Format("3:04 PM")
}
