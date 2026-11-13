package main

import (
	"fmt"
	"strings"

	"fintwind-go/assets"
	"fintwind-go/internal/model"

	"github.com/ELY-Open-Source/CrychicUI/kit"
	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/action"
	"github.com/lobehub/lobe-mygo/pkg/field"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

type contextPanelState struct {
	focus         ui.Handle
	focusOnce     bool
	summaryID     string
	windowSession string
	windowModel   string
	windowValue   string
	windowSource  string
	windowOptions string
}

func (a *app) toggleContextPanel() {
	if a.detail == nil || a.mode == modeSettings || a.mode == modeUsage || a.mode == modeScheduled {
		return
	}
	if a.rightOpen && a.activeKind() == rightPanelContext {
		a.closeSurface(a.activeSurface)
		a.focusComposerOnce = true
		return
	}
	a.mode = modeSessions
	a.rightOpen = true
	a.openSurface(rightPanelContext)
}

func (a *app) effectiveContextUsage() *model.ContextUsage {
	if a.detail == nil {
		return nil
	}
	usage := a.detail.ContextUsage
	window := a.configuredContextWindow()
	if window == 0 {
		return usage
	}
	result := model.ContextUsage{}
	if usage != nil {
		result = *usage
	}
	result.Window = &window
	return &result
}

func contextAction(c *ui.Context, key, label string) ui.Element {
	return action.Button(c.Key(key), tr(label), action.ButtonProps{Variant: action.Default, Size: theme.Small}).Label(tr(label))
}

func contextCard(c *ui.Context, title string, body func()) {
	cardShadow(ui.Column(c).FillWidth().MinWidth(0).Padding(12).Gap(10).
		Radius(cardRadius).Background(palette.raised).Border(1, palette.cardBorder)).Children(func() {
		if title != "" {
			ui.Text(c, tr(title)).FontSize(uiPx(12)).FontWeight(600).TextColor(palette.text)
		}
		body()
	})
}

func contextInspector(c *ui.Context, label, value string) {
	ui.Row(c).FillWidth().MinWidth(0).Gap(8).AlignItems(ui.Center).Children(func() {
		ui.Text(c, tr(label)).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).Shrink(0)
		ui.Text(c, value).Grow(1).MinWidth(0).FontSize(uiPx(11.5)).TextColor(palette.text).
			SingleLine().Ellipsis("…").TextAlign(ui.End).Tooltip(value)
	})
}

// contextStatCard is one KPI mini card inside the Used context card: title
// over a large value. Labels stay the inspector names so automation and
// translations keep working.
func contextStatCard(c *ui.Context, key, title, value string) {
	c.Key("context-stat-" + key)
	ui.Column(c).Grow(1).Basis(110).MinWidth(0).Padding(10, 12).Gap(4).
		Radius(10).Background(palette.overlay).Border(1, palette.cardBorder).
		Label(tr(title)).Description(value).
		Children(func() {
			ui.Text(c, tr(title)).FontSize(uiPx(11)).TextColor(palette.textSecondary).SingleLine().Ellipsis("…")
			kit.Numeric(c, value).FontFeatures("tnum").FontSize(uiPx(18)).FontWeight(600).
				TextColor(palette.text).SingleLine().Ellipsis("…")
		})
}

func (a *app) contextSurface(c *ui.Context) {
	if a.contextPanel.focusOnce {
		a.contextPanel.focusOnce = false
		a.contextPanel.focus.Focus(c)
	}
	ui.Column(c.Key("context-panel")).FillWidth().Grow(1).MinHeight(0).
		Bind(&a.contextPanel.focus).TabStop(true).OnShortcut(ui.Modifiers(0), ui.KeyEscape, func() {
		a.closeSurface(a.activeSurface)
		a.focusComposerOnce = true
	}).Children(func() {
		if a.detail == nil {
			return
		}
		ui.Scroll(c.Key("context-scroll-" + a.detail.ID)).FillWidth().Grow(1).MinHeight(0).Children(func() {
			ui.Column(c).FillWidth().MinWidth(0).Padding(16).Gap(16).Children(func() {
				a.contextUsageCard(c)
				a.contextWindowCard(c)
				a.contextSessionCard(c)
				if state := a.detail.Compaction; state != nil && state.Status == "completed" {
					contextCard(c, "Compaction", func() {
						label := "Completed"
						if state.Reason != nil {
							switch *state.Reason {
							case "auto":
								label = "Automatic"
							case "manual":
								label = "Manual"
							}
						}
						ui.Text(c, tr(label)).FontSize(uiPx(12)).TextColor(palette.textSecondary)
						if a.contextPanel.summaryID != "" {
							contextAction(c, "context-summary", "View summary").OnClick(a.revealContextSummary)
						}
					})
				}
			})
		})
	})
}

func (a *app) contextUsageCard(c *ui.Context) {
	usage := a.effectiveContextUsage()
	percent, measured := contextPercent(usage)
	known := usage != nil && (usage.Measured || usage.Tokens > 0)
	headline, capacity, status, hint := "—", tr("Window unknown"), "Unknown", "Not measured yet"
	color := palette.textSecondary
	if known {
		headline = formatTokens(usage.Tokens)
		hint = "Context window is unknown"
	}
	if usage != nil && usage.Window != nil && *usage.Window > 0 {
		capacity = "/ " + formatTokens(*usage.Window)
		hint = "Context usage is unknown"
		if known {
			hint = trf("%s remaining", formatTokens(*usage.Window-min(*usage.Window, usage.Tokens)))
		}
	}
	if measured {
		status = "Healthy"
		if percent >= 95 {
			status, color, hint = "Full", palette.dangerText, tr("Compact context to continue")
		} else if percent >= 80 {
			status, color, hint = "Near full", palette.warningText, tr("Consider compacting context")
		}
	}
	contextCard(c, "Used context", func() {
		ui.Row(c).FillWidth().MinWidth(0).Gap(8).AlignItems(ui.Baseline).Wrap().Children(func() {
			ui.Text(c, headline).FontSize(uiPx(28)).FontWeight(600).TextColor(palette.text)
			ui.Text(c, capacity).FontSize(uiPx(12)).TextColor(palette.textSecondary)
			ui.Text(c, tr(status)).FontSize(uiPx(11)).TextColor(color)
		})
		if measured {
			ui.Text(c, formatPercent(percent)+" · "+tr(hint)).FontSize(uiPx(11)).TextColor(color).Wrap()
			usageProgress(c, float32(min(percent, 100)/100), usageFillColor(percent, true))
		} else {
			ui.Text(c, tr(hint)).FontSize(uiPx(11)).TextColor(palette.textTertiary).Wrap()
		}
		total, cache, first := "—", "—", "—"
		if usage != nil {
			if usage.TotalTokens != nil {
				total = formatTokens(*usage.TotalTokens)
			}
			if usage.PromptTokens != nil && *usage.PromptTokens > 0 {
				read := uint64(0)
				if usage.CacheRead != nil {
					read = *usage.CacheRead
				}
				cache = formatPercent(float64(min(read, *usage.PromptTokens)) * 100 / float64(*usage.PromptTokens))
			}
			if usage.FirstTokenMs != nil && usage.FirstTokenTurns != nil && *usage.FirstTokenTurns > 0 {
				ms := *usage.FirstTokenMs / *usage.FirstTokenTurns
				first = fmt.Sprintf("%dms", ms)
				if ms >= 1000 {
					first = fmt.Sprintf("%.1fs", float64(ms)/1000)
				}
			}
		}
		ui.Row(c).FillWidth().MinWidth(0).Gap(8).Wrap().Children(func() {
			contextStatCard(c, "total", "Total tokens", total)
			contextStatCard(c, "cache", "Cache hit rate", cache)
			contextStatCard(c, "first", "First token latency", first)
		})
		contextComposition(c, usage)
		a.contextCompactControl(c)
	})
}

func (a *app) contextWindowCard(c *ui.Context) {
	contextCard(c, "Window", func() {
		modelID := ""
		if a.detail.Model != nil {
			modelID = *a.detail.Model
			contextInspector(c, "Model", modelID)
		}
		model := a.currentProviderModel()
		if model != nil && len(model.ContextWindows) > 0 {
			value, label := contextWindowSelection(a.detail.ContextWindow, model)
			if len(model.ContextWindows) == 1 {
				contextInspector(c, "Context size", label)
				return
			}
			state := &a.contextPanel
			optionIDs := value + "\x00"
			for _, option := range model.ContextWindows {
				optionIDs += option.ID + "\x00"
			}
			source := ""
			if a.detail.ContextWindow != nil {
				source = *a.detail.ContextWindow
			}
			if state.windowSession != a.detail.ID || state.windowModel != modelID || state.windowSource != source || state.windowOptions != optionIDs {
				state.windowSession, state.windowModel, state.windowSource, state.windowValue = a.detail.ID, modelID, source, value
				state.windowOptions = optionIDs
			}
			options := make([]field.SelectOption, 0, len(model.ContextWindows))
			for _, option := range model.ContextWindows {
				options = append(options, field.SelectOption{Value: option.ID, Label: option.Label})
			}
			ui.Text(c, tr("Context size")).FontSize(uiPx(11.5)).TextColor(palette.textSecondary)
			field.Select(c.Key("context-window-"+a.detail.ID+"-"+modelID), &state.windowValue,
				field.SelectProps{Options: options, Size: theme.Small, Disabled: a.sessionActionBlocksInput(a.detail.ID)}).
				FillWidth().Label(tr("Context size")).OnChange(func() { a.chooseContextWindow(state.windowValue) })
		} else if usage := a.effectiveContextUsage(); usage != nil && usage.Window != nil && *usage.Window > 0 {
			contextInspector(c, "Context size", formatTokens(*usage.Window))
		}
	})
}

func (a *app) contextSessionCard(c *ui.Context) {
	id := nativeSessionID(a.detail.ProviderCursor)
	if id == "" && a.detail.NativeSessionID != nil {
		id = *a.detail.NativeSessionID
	}
	contextCard(c, "OpenCode session ID", func() {
		if id == "" {
			ui.Text(c, "—").FontSize(uiPx(12)).TextColor(palette.textTertiary)
			return
		}
		services := c.Services()
		ui.Row(c).FillWidth().MinWidth(0).Gap(4).AlignItems(ui.Center).Children(func() {
			ui.Text(c, id).MinWidth(0).Shrink(1).FontSize(uiPx(11.5)).TextColor(palette.text).
				SingleLine().Ellipsis("…").Tooltip(id)
			action.ActionIcon(c.Key("context-copy-session"), assets.SVG("copy"), action.ActionIconProps{
				Size: theme.Small,
			}).Label(tr("Copy session ID")).Shrink(0).OnClick(func() {
				services.WriteClipboard(id)
				a.status = tr("Copied")
			})
		})
	})
}

func (a *app) refreshContextSummary() {
	a.contextPanel.summaryID = ""
	if a.detail == nil || a.detail.Compaction == nil || a.detail.Compaction.Summary == nil {
		return
	}
	summary := strings.TrimSpace(*a.detail.Compaction.Summary)
	for i := len(a.detail.Messages) - 1; i >= 0; i-- {
		message := &a.detail.Messages[i]
		if message.Role == "compaction" && strings.TrimSpace(message.Content) == summary {
			a.contextPanel.summaryID = message.ID
			return
		}
	}
}

func (a *app) revealContextSummary() {
	if a.detail == nil {
		return
	}
	id := a.contextPanel.summaryID
	for i := range a.detail.Messages {
		message := &a.detail.Messages[i]
		if message.ID != id {
			continue
		}
		if message.TurnID != nil {
			if a.expandedTurns == nil {
				a.expandedTurns = map[string]bool{}
			}
			a.expandedTurns[*message.TurnID] = true
		}
		if a.expandedCompaction == nil {
			a.expandedCompaction = map[string]bool{}
		}
		a.expandedCompaction[id] = true
		a.rebuildTranscript()
		for row, entry := range a.transcriptRows {
			if entry.Kind == transcriptMessage && entry.MessageIndex == i {
				a.messageList.FollowEnd = false
				a.messageList.ScrollTo(row, ui.Center)
				a.focusComposerOnce = true
				return
			}
		}
	}
}
