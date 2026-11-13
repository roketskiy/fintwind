package main

import (
	"fmt"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/action"
	"github.com/lobehub/lobe-mygo/pkg/overlay"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

const (
	sessionCardHeight   = 52.0
	sessionAvatarSize   = 36.0
	sessionAvatarInner  = 26.0
	sessionAvatarIcon   = 14.0
	sessionRowGap       = 2.0
	actionRowHeight     = 32.0
	actionRowGap        = 4.0
	searchBottomGap     = 10.0
	projectCardHeight   = 28.0
	projectCardGap      = 2.0
	groupVisibleDefault = 6
	groupVisibleStep    = 6
)

func (a *app) sidebar(c *ui.Context) {
	side := ui.Column(c).Width(a.sidebarWidth).FillHeight().Shrink(0).
		Background(palette.sidebar).Radius(workspaceRadius).Clip()
	side = sidebarShadow(side)
	side.Children(func() {
		ui.Column(c.Key("sidebar-actions")).PaddingX(10).Gap(actionRowGap).Children(func() {
			a.sidebarAction(c.Key("new-task"), "compose", tr("New task")).OnClick(func() {
				a.newTab()
				a.revealSidebarSession(a.selectedID)
			})
			a.sidebarAction(c.Key("add-project"), "plus", tr("Add project")).OnClick(a.pickProjectFolder)
			a.sidebarAction(c.Key("search"), "search", tr("Search")).OnClick(func() {
				a.paletteOpen = true
				a.paletteQuery = ""
				a.paletteIndex = 0
			})
		})
		ui.Row(c).Height(searchBottomGap).Shrink(0)
		rows := a.rows
		first, _ := a.sidebarList.Visible()
		ui.Column(c.Key("sidebar-history")).Grow(1).MinWidth(0).MinHeight(0).Clip().Children(func() {
			// Row controls own focus and activation. The virtual list only scrolls,
			// so project headers and spacers never become a selected conversation.
			ui.List(c, &a.sidebarList, len(rows)).Grow(1).Padding(0, 10).
				ItemKey(func(index int) any { return rows[index].key() }).
				Rows(func(row ui.ListRow) { a.sidebarRowView(row.Context, rows, row.Index) })
			if first > 0 {
				ui.Row(c).Absolute().Left(0).Right(0).Top(0).Height(1).
					Background(palette.border).PassThrough()
			}
		})
		a.sidebarResizeHandle(c)
	})
}

// Keep library keyboard/focus behavior while applying the GPUI card geometry.
func sidebarButton(c *ui.Context, label string) ui.Element {
	b := action.Button(c, "", action.ButtonProps{Variant: action.Text, Size: theme.Middle}).
		Label(label).Row().MinWidth(0).Shrink(0).Border(0, ui.Transparent).
		Cursor(ui.CursorDefault).Background(ui.Transparent).TextColor(palette.text).
		FocusRingColor(palette.accent).Justify(ui.Start).AlignItems(ui.Center)
	if b.Hovered() {
		b.Background(palette.sidebarItem)
	}
	if b.Pressed() {
		b.Background(palette.overlayStrong)
	}
	return b
}

func (a *app) sidebarAction(c *ui.Context, glyph, label string) ui.Element {
	b := sidebarButton(c, label).FillWidth().Height(actionRowHeight).Padding(0, 4).Radius(7).Gap(10)
	b.Children(func() {
		ui.Row(c).Size(20, 20).Shrink(0).Center().Children(func() {
			icon(c, glyph, 16, palette.textSecondary)
		})
		ui.Text(c, label).FontSize(uiPx(13)).TextColor(palette.textSecondary).SingleLine()
	})
	return b
}

func (a *app) sidebarRowView(c *ui.Context, rows []sidebarRow, index int) {
	item := rows[index]
	switch item.Kind {
	case rowGroupHeader:
		a.sidebarGroupHeader(c, item.Project)
	case rowSession:
		// Session slices can be replaced while the list retains its anchor.
		if session := a.byID[item.ID]; session != nil {
			a.sidebarSession(c, rows, index, session)
		}
	case rowShowMore:
		a.sidebarShowMore(c, item.Project, item.Remaining)
	case rowSpacer:
		ui.Row(c).Height(10).PassThrough()
	}
}

func (a *app) setSidebarCollapsed(id string, collapsed bool) {
	if a.collapsed == nil {
		a.collapsed = make(map[string]bool)
	}
	a.collapsed[id] = collapsed
	a.rebuildRows()
}

func (a *app) sidebarGroupHeader(c *ui.Context, project *model.Project) {
	id, name := project.ID, project.Name
	if name == "No project" || name == "Unknown project" {
		name = tr(name)
	}
	ui.Column(c).FillWidth().Padding(0, 0, projectCardGap, 0).Children(func() {
		card := ui.Row(c).FillWidth().Height(projectCardHeight).Padding(0, 4).
			Radius(8).Gap(2).AlignItems(ui.Center)
		if card.Hovered() {
			card.Background(palette.sidebarItem)
		}
		card.Children(func() {
			toggle := sidebarButton(c.Key("toggle"), name).Grow(1).Height(projectCardHeight).
				Padding(0, 4).Radius(6).Gap(5).Background(ui.Transparent)
			toggle.Children(func() {
				icon(c, "folder", 12, palette.textGhost)
				ui.Text(c, name).Grow(1).MinWidth(0).FontSize(uiPx(12.5)).FontWeight(500).
					TextColor(palette.textSecondary).SingleLine().Ellipsis("…")
				glyph := "chevron-down"
				if a.collapsed[id] {
					glyph = "chevron-right"
				}
				icon(c, glyph, 11, palette.textGhost)
			})
			toggle.OnClick(func() { a.setSidebarCollapsed(id, !a.collapsed[id]) }).
				OnShortcut(0, ui.KeyLeft, func() { a.setSidebarCollapsed(id, true) }).
				OnShortcut(0, ui.KeyRight, func() { a.setSidebarCollapsed(id, false) })
			b := sidebarButton(c.Key("new-task"), tr("New task")).Size(28, 28).
				Padding(0).Radius(7).Center()
			b.Children(func() { icon(c, "compose", 14, palette.textTertiary) })
			b.OnClick(func() {
				a.lastProject = id
				a.newTab()
				a.revealSidebarSession(a.selectedID)
			})
			b.Disabled(a.projectForID(id) == nil)
		})
		if overlay.ContextMenu(c, card, overlay.DropdownMenuProps{Items: []overlay.MenuItem{
			{Key: "remove", Label: tr("Remove from app"), Disabled: a.projectForID(id) == nil},
		}}) == "remove" {
			a.projectRemoveConfirm = id
		}
		if a.projectRemoveConfirm == id && a.projectForID(id) != nil {
			a.projectRemoveConfirmBar(c, project)
		}
	})
}

func (a *app) sidebarSession(c *ui.Context, rows []sidebarRow, index int, session *model.Session) {
	id := session.ID
	renaming := a.renamingSession == id
	ui.Row(c).FillWidth().Height(sessionCardHeight+sessionRowGap).Padding(0, 0, 0, 22).
		Children(func() {
			// The rail stays outside the hit target and never captures its input.
			bottom := float32(0)
			if index+1 == len(rows) || rows[index+1].Kind != rowSession {
				bottom = sessionRowGap
			}
			ui.Row(c).Absolute().Left(14).Top(0).Bottom(bottom).Width(1).
				Background(palette.border).PassThrough()
			if id == a.selectedID {
				ui.Row(c).Absolute().Left(13).Top((sessionCardHeight-20)/2).Size(3, 20).
					Radius(1.5).Background(palette.text).PassThrough()
			}
			// An editor must not be nested inside a button that also handles Enter.
			var card ui.Element
			if renaming {
				card = ui.Row(c.Key("rename-card"))
			} else {
				card = sidebarButton(c.Key("card"), sidebarSessionTitle(session))
			}
			card.FillWidth().Height(sessionCardHeight).Padding(0, 8).Radius(8).Gap(8).AlignItems(ui.Center)
			if id == a.selectedID || card.Hovered() {
				card.Background(palette.sidebarItem)
			}
			card.Children(func() {
				a.sessionAvatar(c, session)
				ui.Column(c).Grow(1).MinWidth(0).Gap(2).Justify(ui.Center).Children(func() {
					ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
						if renaming {
							a.sessionRenameCell(c)
						} else {
							ui.Text(c, sidebarSessionTitle(session)).Grow(1).MinWidth(0).FontSize(uiPx(12.5)).FontWeight(400).
								TextColor(palette.text).SingleLine().Ellipsis("…")
						}
						switch session.Status {
						case "waiting":
							icon(c, "alert", 12, palette.warning).Tooltip(tr("Waiting"))
						case "failed":
							tip := latestSystemError(session)
							if current := a.eventSession(id); current != nil {
								if detailError := latestSystemError(current); detailError != "" {
									tip = detailError
								}
							}
							if tip == "" {
								tip = tr("Failed")
							}
							icon(c, "x", 12, palette.danger).Tooltip(tip)
						}
					})
					timedSession := session
					if current := a.eventSession(id); current != nil {
						timedSession = current
					}
					label := sessionTimeLabel(timedSession, time.Now())
					if timedSession.Busy() {
						c.After(time.Second)
					} else if timedSession.LastReplyAt != nil {
						c.After(time.Minute)
					}
					branch := a.branches[session.ProjectID].label
					if session.Workspace.Kind == "worktree" {
						branch = session.Workspace.Branch
					}
					if label != "" || branch != "" {
						ui.Row(c).FillWidth().MinWidth(0).Gap(8).AlignItems(ui.Center).Children(func() {
							if label != "" {
								color := palette.textMuted
								if session.Busy() {
									color = palette.textTertiary
								}
								ui.Text(c, label).FontSize(uiPx(11)).TextColor(color).Shrink(0).SingleLine()
							}
							if branch != "" {
								ui.Row(c).Grow(1).MinWidth(0).Gap(5).AlignItems(ui.Center).Children(func() {
									icon(c, "git-branch", 11, palette.textGhost)
									ui.Text(c, branch).Grow(1).MinWidth(0).FontSize(uiPx(11)).
										TextColor(palette.textMuted).SingleLine().Ellipsis("…")
								})
							}
						})
					}
				})
			})
			if renaming {
				if card.PressedOutside() {
					a.commitSessionRename()
				}
				return
			}
			card.OnClick(func() { a.openSession(id) })
			switch overlay.ContextMenu(c, card, overlay.DropdownMenuProps{Items: []overlay.MenuItem{
				{Key: "rename", Label: tr("Rename")},
				{Divider: true},
				{Key: "remove", Label: tr("Remove"), Danger: true},
			}}) {
			case "rename":
				a.beginSessionRename(id)
			case "remove":
				a.removeSession(id)
			}
		})
}

func (a *app) sessionAvatar(c *ui.Context, session *model.Session) {
	ui.Row(c).Size(sessionAvatarSize, sessionAvatarSize).Shrink(0).Center().Children(func() {
		status := session.Status
		if status == "connecting" || status == "working" {
			ui.Row(c).Absolute().Left(0).Top(0).Size(sessionAvatarSize, sessionAvatarSize).
				Center().PassThrough().Children(func() {
				halo := icon(c, "loader-circle", sessionAvatarSize, palette.accent)
				if !c.Preferences().ReduceMotion {
					phase := float32(time.Now().UnixMilli()%1200) / 1200
					halo.Rotate(phase * 360)
					c.After(time.Second / 30)
				}
			})
		}
		ui.Row(c).Size(sessionAvatarInner, sessionAvatarInner).Radius(13).
			Background(palette.raised).Border(1, palette.borderStrong).Center().Children(func() {
			name := companyIcon(session)
			if name == "" {
				name = companyIconFor(session.Provider)
			}
			if !activeAppearance.dark {
				name = strings.TrimSuffix(name, "-dark")
			}
			if name == "" {
				name = "terminal"
			}
			markIcon(c, name, sessionAvatarIcon, companyColor(name))
		})
	})
}

func (a *app) sidebarShowMore(c *ui.Context, project *model.Project, remaining int) {
	ui.Row(c).FillWidth().Padding(0, 0, 0, 22).Children(func() {
		b := sidebarButton(c, tr("Show more")).FillWidth().Height(actionRowHeight).
			Padding(0, 8).Radius(6).Gap(6)
		b.Children(func() {
			icon(c, "chevron-down", 11, palette.textGhost)
			ui.Text(c, tr("Show more")).Grow(1).MinWidth(0).FontSize(uiPx(12.5)).
				TextColor(palette.textSecondary).SingleLine()
			ui.Text(c, strings.ReplaceAll(tr("%{count} hidden"), "%{count}", fmt.Sprint(remaining))).FontSize(uiPx(11)).
				TextColor(palette.textMuted).Shrink(0)
		})
		b.OnClick(func() {
			if a.visible == nil {
				a.visible = make(map[string]int)
			}
			a.visible[project.ID] = a.sidebarGroupVisible(project.ID) + min(remaining, groupVisibleStep)
			a.rebuildRows()
		})
	})
}

func sessionTimeLabel(session *model.Session, now time.Time) string {
	if session.Busy() && len(session.Turns) > 0 {
		turn := session.Turns[len(session.Turns)-1]
		if turn.Status == "running" {
			elapsed := max(int64(0), now.Unix()-int64(turn.StartedAt))
			return strings.ReplaceAll(tr("Working for %{elapsed}"), "%{elapsed}", formatWorkingElapsed(uint64(elapsed)))
		}
	}
	if session.LastReplyAt == nil {
		return ""
	}
	delta := max(int64(0), now.Unix()-int64(*session.LastReplyAt))
	switch {
	case delta < 60:
		return tr("just now")
	case delta < 3600:
		return strings.ReplaceAll(tr("%{count}m"), "%{count}", fmt.Sprint(delta/60))
	case delta < 86400:
		return strings.ReplaceAll(tr("%{count}h"), "%{count}", fmt.Sprint(delta/3600))
	default:
		return strings.ReplaceAll(tr("%{count}d"), "%{count}", fmt.Sprint(delta/86400))
	}
}

func sidebarSessionTitle(session *model.Session) string {
	title := session.DisplayTitle()
	if title == "New task" {
		return tr(title)
	}
	if session.Title == "New task" && session.AutoTitle != nil && *session.AutoTitle == title && strings.HasPrefix(title, "New task (") && strings.HasSuffix(title, ")") {
		number := strings.TrimSuffix(strings.TrimPrefix(title, "New task ("), ")")
		if n, err := strconv.Atoi(number); err == nil && n > 0 && strconv.Itoa(n) == number {
			return fmt.Sprintf(tr("New task (%d)"), n)
		}
	}
	return title
}

func latestSystemError(session *model.Session) string {
	for i := len(session.Messages) - 1; i >= 0; i-- {
		if session.Messages[i].Role == "system" {
			return session.Messages[i].Content
		}
	}
	return ""
}

func companyColor(name string) ui.Color {
	switch name {
	case "company-deepseek":
		return hex(0x4D6BFE)
	case "company-anthropic":
		return hex(0xD97757)
	case "company-openai":
		return hex(0x10A37F)
	case "company-google":
		return hex(0x4285F4)
	case "company-qwen":
		return hex(0x6B4EFF)
	case "company-mistral":
		return hex(0xFF7000)
	case "company-meta":
		return hex(0x0866FF)
	default:
		return palette.text
	}
}

func companyIcon(session *model.Session) string {
	if session.Model == nil {
		return ""
	}
	return companyIconFor(*session.Model)
}

// companyIconFor mirrors model_mark: the model's company wins over the
// serving provider, and monochrome marks swap to their dark variant.
func companyIconFor(value string) string {
	lower := strings.ToLower(value)
	has := func(tokens ...string) bool {
		for _, token := range tokens {
			if strings.Contains(lower, token) {
				return true
			}
		}
		return false
	}
	switch {
	case has("grok", "xai"):
		return "company-xai-dark"
	case has("claude", "anthropic"):
		return "company-anthropic"
	case has("gpt", "o1", "o3", "o4", "codex", "davinci", "chatgpt", "openai"):
		return "company-openai-dark"
	case has("gemini", "gemma", "google"):
		return "company-google"
	case has("deepseek"):
		return "company-deepseek"
	case has("qwen", "tongyi", "qwq", "qvq", "alibaba"):
		return "company-qwen"
	case has("kimi", "moonshot", "moonshotai"):
		return "company-moonshot-dark"
	case has("glm", "chatglm", "zhipu", "zhipuai", "zai", "z.ai"):
		return "company-zai"
	case has("llama", "meta", "muse"):
		return "company-meta"
	case has("mistral", "mixtral", "codestral", "pixtral", "magistral", "ministral", "devstral", "voxtral"):
		return "company-mistral"
	case has("minimax", "hailuo", "abab"):
		return "company-minimax"
	case has("doubao", "bytedance", "ui-tars"):
		return "company-bytedance"
	case has("phi", "microsoft", "mai"):
		return "company-microsoft"
	case has("nova", "amazon"):
		return "company-amazon"
	case has("sonar", "perplexity", "r1-1776"):
		return "company-perplexity"
	case has("nemotron", "nvidia"):
		return "company-nvidia"
	case has("longcat", "meituan"):
		return "company-longcat"
	case has("stepfun", "step"):
		return "company-stepfun"
	case has("hy3", "hy4", "hunyuan", "tencent"):
		return "company-tencent"
	case has("mimo", "xiaomi"):
		return "company-xiaomi"
	case has("beam", "reflection"):
		return "company-reflection-dark"
	case has("opencode"):
		return "provider-opencode"
	}
	return ""
}
