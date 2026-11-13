package main

import (
	"encoding/json"
	"fmt"
	"strings"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/transfer"
	"github.com/egoist/mygo/ui"
)

// Values from transcript_view.rs.
const (
	activityOpacity          = 0.70
	activityClusterPreview   = 8
	activitySectionMaxHeight = 400.0
	activityDetailMaxHeight  = 800.0
	activityDiffMaxHeight    = 400.0
)

const (
	clusterCollapsed = iota
	clusterRecent
	clusterAll
)

const (
	sectionCommand   = "command"
	sectionArguments = "arguments"
	sectionOutput    = "output"
	sectionDetail    = "detail"
)

type activitySection struct {
	Kind    string
	Label   string
	Content string
}

// activityCluster renders one transcript block: the summary line that toggles
// the card list, plus the cards themselves.
func (a *app) activityCluster(c *ui.Context, blockIndex int, liveTurn bool, latestGroup bool) {
	session := a.detail
	if session == nil || blockIndex < 0 || blockIndex >= len(session.TranscriptBlocks) {
		return
	}
	block := &session.TranscriptBlocks[blockIndex]
	activities := block.Activities
	liveReasoningID := a.liveReasoningID(activities, blockIndex, liveTurn)
	state := a.clusterState(blockIndex, liveTurn, latestGroup)
	expanded := state != clusterCollapsed
	headerTitle := activityHeaderTitle(activities, liveTurn, liveReasoningID)

	ui.Column(c).FillWidth().MinWidth(0).Gap(4).Children(func() {
		header := ui.ButtonBase(c).Label(headerTitle).Opacity(activityOpacity).
			FillWidth().MinWidth(0).Height(30).Gap(6).Justify(ui.Start).AlignItems(ui.Center)
		header.Children(func() {
			hovered := header.Hovered() || header.Pressed()
			color := palette.textSecondary
			if hovered {
				color = palette.text
			}
			ui.Row(c).MinWidth(0).Shrink(1).Children(func() {
				ui.Text(c, headerTitle).FontSize(uiPx(13)).FixedLineHeight(uiPx(17)).
					FontWeight(500).TextColor(color).SingleLine().Ellipsis("...")
			})
			chevron := "chevron-right"
			if expanded {
				chevron = "chevron-down"
			}
			icon(c, chevron, 11, palette.textTertiary)
		})
		if header.Clicked() {
			if expanded {
				a.clusterDisclosure[blockIndex] = clusterCollapsed
			} else {
				a.clusterDisclosure[blockIndex] = clusterRecent
			}
		}
		if !expanded {
			return
		}
		ui.Column(c).FillWidth().MinWidth(0).Padding(0, 0, 2, 18).
			Gap(8).Children(func() {
			ui.Row(c).Absolute().Left(6).Top(0).Bottom(0).Width(1).
				Background(palette.border)
			display := mergeConsecutiveReasoning(activities, liveReasoningID,
				a.mergesReasoning())
			start := 0
			switch state {
			case clusterRecent:
				if len(display) > activityClusterPreview {
					start = len(display) - activityClusterPreview
				}
			case clusterAll:
				start = 0
			}
			earlier := len(display) - activityClusterPreview
			if earlier < 0 {
				earlier = 0
			}
			if earlier > 0 {
				showingAll := state == clusterAll
				label := fmt.Sprintf(tr("Show %d earlier activities"), earlier)
				if showingAll {
					label = tr("Hide earlier activities")
				}
				more := ui.ButtonBase(c).Label(label).FillWidth().MinWidth(0).
					Height(28).Padding(0, 6).Radius(4).Gap(6).Justify(ui.Start).AlignItems(ui.Center)
				if more.Hovered() || more.Pressed() {
					more.Background(palette.overlay)
				}
				more.Children(func() {
					chevron := "chevron-down"
					if showingAll {
						chevron = "chevron-up"
					}
					icon(c, chevron, 11, palette.textTertiary)
					color := palette.textTertiary
					if more.Hovered() || more.Pressed() {
						color = palette.text
					}
					ui.Text(c, label).FontSize(uiPx(12)).TextColor(color)
				})
				if more.Clicked() {
					if showingAll {
						a.clusterDisclosure[blockIndex] = clusterRecent
					} else {
						a.clusterDisclosure[blockIndex] = clusterAll
					}
				}
			}
			for index := start; index < len(display); index++ {
				merged := display[index]
				a.activityCard(c, merged)
			}
		})
	})
}

// clusterState resolves a block's disclosure, opening the newest live group.
func (a *app) clusterState(blockIndex int, liveTurn bool, latestGroup bool) int {
	if state, ok := a.clusterDisclosure[blockIndex]; ok {
		return state
	}
	if liveTurn && latestGroup {
		return clusterRecent
	}
	return clusterCollapsed
}

func (a *app) liveReasoningID(activities []model.ActivityItem, blockIndex int, liveTurn bool) string {
	session := a.detail
	if session == nil || !liveTurn || blockIndex+1 != len(session.TranscriptBlocks) {
		return ""
	}
	for index := len(activities) - 1; index >= 0; index-- {
		activity := &activities[index]
		if activity.Reasoning != nil {
			if activity.Complete {
				return ""
			}
			return activity.ID
		}
	}
	return ""
}

// mergesReasoning reports whether this model streams short summary fragments
// instead of one full chain of thought.
func (a *app) mergesReasoning() bool {
	if a.detail == nil || a.detail.Model == nil {
		return false
	}
	return summaryOnlyReasoningModel(*a.detail.Model)
}

func summaryOnlyReasoningModel(value string) bool {
	lower := strings.ToLower(strings.TrimSpace(value))
	if lower == "" {
		return false
	}
	provider := ""
	id := lower
	if index := strings.Index(lower, "/"); index >= 0 {
		provider = lower[:index]
		id = lower[index+1:]
	}
	if provider == "anthropic" || provider == "openai" {
		return true
	}
	for _, prefix := range []string{"claude", "gpt", "o1", "o3", "o4"} {
		if strings.HasPrefix(id, prefix) {
			return true
		}
	}
	return false
}

type mergedActivity struct {
	Item       model.ActivityItem
	GroupCount int
	Live       bool
}

// mergeConsecutiveReasoning collapses consecutive real thoughts into one card
// for summary-only models, dropping empty ones.
func mergeConsecutiveReasoning(activities []model.ActivityItem, liveReasoningID string, merge bool) []mergedActivity {
	if !merge {
		result := make([]mergedActivity, 0, len(activities))
		for index := range activities {
			result = append(result, mergedActivity{
				Item:       activities[index],
				GroupCount: 1,
				Live:       liveReasoningID != "" && liveReasoningID == activities[index].ID,
			})
		}
		return result
	}
	result := make([]mergedActivity, 0, len(activities))
	var group *mergedActivity
	hasGroup := false
	flush := func() {
		if !hasGroup {
			return
		}
		if group.Live && group.Item.ID == "" {
			group.Item.ID = liveReasoningID
		}
		result = append(result, *group)
		hasGroup = false
	}
	for index := range activities {
		activity := activities[index]
		hasThought := activity.Kind == model.KindReasoning &&
			activity.Reasoning != nil && strings.TrimSpace(activity.Reasoning.Content) != ""
		if !hasThought {
			flush()
			if activity.Kind != model.KindReasoning {
				result = append(result, mergedActivity{
					Item:       activity,
					GroupCount: 1,
					Live:       liveReasoningID != "" && liveReasoningID == activity.ID,
				})
			}
			continue
		}
		live := liveReasoningID != "" && liveReasoningID == activity.ID
		if hasGroup {
			content := strings.TrimSpace(activity.Reasoning.Content)
			if strings.TrimSpace(group.Item.Reasoning.Content) != "" && content != "" {
				group.Item.Reasoning.Content += "\n\n"
			}
			group.Item.Reasoning.Content += content
			if activity.Reasoning.StartedAtMs < group.Item.Reasoning.StartedAtMs {
				group.Item.Reasoning.StartedAtMs = activity.Reasoning.StartedAtMs
			}
			if activity.Reasoning.FinishedAtMs > group.Item.Reasoning.FinishedAtMs {
				group.Item.Reasoning.FinishedAtMs = activity.Reasoning.FinishedAtMs
			}
			group.Item.Complete = group.Item.Complete && activity.Complete
			group.Item.Failed = group.Item.Failed || activity.Failed
			group.GroupCount++
			group.Live = group.Live || live
			if live {
				group.Item.ID = activity.ID
				group.Item.SourceID = activity.SourceID
			}
			continue
		}
		item := activity
		if item.Reasoning != nil {
			item.Reasoning.Content = strings.TrimSpace(activity.Reasoning.Content)
		}
		group = &mergedActivity{Item: item, GroupCount: 1, Live: live}
		hasGroup = true
	}
	flush()
	return result
}

// activityCard renders one activity: the row that names the action and, when
// the activity carries more than a title, the expandable detail below it.
func (a *app) activityCard(c *ui.Context, merged mergedActivity) {
	activity := merged.Item
	reasoning := activity.Reasoning
	reasoningLive := merged.Live || reasoning != nil && !activity.Complete && !activity.Failed
	hasDetail := false
	if reasoning != nil {
		hasDetail = strings.TrimSpace(reasoning.Content) != ""
	} else {
		hasDetail = len(activityDisclosureSections(&activity)) > 0 || activityShowsDiff(&activity)
	}
	expanded := false
	if hasDetail {
		if value, ok := a.expandedActivities[activity.ID]; ok {
			expanded = value
		} else {
			expanded = reasoningLive
		}
	}

	label := activityActionLabel(&activity)
	rowDetail := activityRowDetail(&activity, reasoningLive)
	if reasoning != nil && merged.GroupCount > 1 {
		rowDetail = reasoningGroupTitle(reasoning, reasoningLive, merged.GroupCount)
	}
	if reasoning != nil && strings.TrimSpace(rowDetail) != "" {
		label = rowDetail
		rowDetail = ""
	}
	if strings.TrimSpace(rowDetail) == "" {
		rowDetail = activityPreview(&activity)
	}
	running := reasoningLive || (!activity.Complete && !activity.Failed)
	rail := palette.textTertiary
	if running {
		rail = palette.accent
	}
	statsAdditions, statsDeletions, hasStats := activityFileChangeStats(&activity)

	card := ui.Column(c).FillWidth().MinWidth(0).Clip().
		BorderWidth(0, 0, 0, 3).BorderColor(rail).Background(palette.raised)
	card.Children(func() {
		row := ui.ButtonBase(c).Label(label).FillWidth().Height(32).Padding(0, 12).
			Gap(8).Justify(ui.Start).AlignItems(ui.Center)
		if hasDetail {
			if row.Pressed() {
				row.Background(palette.overlayStrong.Over(palette.raised))
			} else if row.Hovered() {
				row.Background(palette.overlay.Over(palette.raised))
			}
		}
		row.Children(func() {
			ui.Text(c, label).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(17)).
				FontWeight(600).TextColor(palette.textSecondary).Shrink(0)
			if rowDetail != "" {
				ui.Row(c).Grow(1).MinWidth(0).Children(func() {
					ui.Text(c, rowDetail).FontSize(uiPx(12.5)).FixedLineHeight(uiPx(17)).
						TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
				})
			} else {
				ui.Row(c).Grow(1)
			}
			if hasStats {
				ui.Text(c, fmt.Sprintf(tr("+%d"), statsAdditions)).FontSize(uiPx(12.5)).
					TextColor(palette.successText).Shrink(0)
				ui.Text(c, fmt.Sprintf("-%d", statsDeletions)).FontSize(uiPx(12.5)).
					TextColor(palette.dangerText).Shrink(0)
			}
			if hasDetail {
				chevron := "chevron-right"
				if expanded {
					chevron = "chevron-down"
				}
				icon(c, chevron, 11, palette.textTertiary)
			}
			if !hasDetail && reasoning == nil {
				if activity.Failed {
					icon(c, "x", 11, palette.danger)
				} else if !activity.Complete {
					ui.Row(c).Size(5, 5).Radius(2.5).Background(palette.accent)
				}
			}
		})
		if hasDetail && row.Clicked() {
			a.expandedActivities[activity.ID] = !expanded
		}
		if expanded {
			if reasoning != nil && hasDetail {
				ui.Column(c).FillWidth().MinWidth(0).MaxHeight(activitySectionMaxHeight).
					Clip().Padding(8, 12).Children(func() {
					ui.Row(c).Absolute().Left(0).Right(0).Top(0).Height(1).
						Background(palette.borderStrong)
					a.renderMarkdown(c, "reasoning:"+activity.ID, reasoning.Content, &a.mdReasoning, reasoningLive)
				})
			} else if activityShowsDiff(&activity) {
				a.activityDiff(c, &activity)
			} else {
				sections := activityDisclosureSections(&activity)
				if len(sections) > 0 || len(activity.ImageURLs) > 0 {
					ui.Column(c).FillWidth().MinWidth(0).MaxHeight(activityDetailMaxHeight).
						Clip().Padding(8, 12).Gap(8).Children(func() {
						ui.Row(c).Absolute().Left(0).Right(0).Top(0).Height(1).
							Background(palette.borderStrong)
						for _, section := range sections {
							ui.Column(c).FillWidth().MinWidth(0).Gap(3).Children(func() {
								if section.Label != "" {
									header := ui.Row(c).Height(24).FillWidth().
										Justify(ui.SpaceBetween).AlignItems(ui.Center)
									header.Children(func() {
										ui.Text(c, section.Label).FontSize(uiPx(10.5)).
											FixedLineHeight(uiPx(16)).FontWeight(500).
											TextColor(palette.textSecondary)
									})
									if section.Content != "" {
										copyButton := ui.ButtonBase(c).Label(tr("Copy ")+strings.ToLower(section.Label)).
											Size(24, 24).Radius(6).Center()
										if copyButton.Hovered() || copyButton.Pressed() {
											copyButton.Background(palette.overlay)
										}
										header.Children(func() {
											copyButton.Children(func() {
												icon(c, "copy", 13, palette.textGhost)
											})
										})
										if copyButton.Clicked() {
											_ = mygo.Clipboard.Write(transfer.TextData(section.Content))
										}
									}
								}
								if section.Content != "" {
									ui.Text(c, section.Content).FontSize(codePx(10.5)).FixedLineHeight(codePx(16)).
										Font(monoFont()).TextColor(palette.textSecondary).Wrap()
								}
							})
						}
						for _, imageURL := range activity.ImageURLs {
							ui.Text(c, imageURL).FontSize(uiPx(10.5)).TextColor(palette.textTertiary).
								SingleLine().Ellipsis("...")
						}
					})
				}
			}
		}
	})
}

// activityDiff renders an edit's unified diff inside its card.
func (a *app) activityDiff(c *ui.Context, activity *model.ActivityItem) {
	ui.Column(c).FillWidth().MinWidth(0).MaxHeight(activityDiffMaxHeight).Clip().
		Children(func() {
			ui.Row(c).Absolute().Left(0).Right(0).Top(0).Height(1).
				Background(palette.borderStrong)
			for _, change := range activity.FileChanges {
				singleFile := len(activity.FileChanges) < 2
				ui.Column(c).FillWidth().MinWidth(0).Children(func() {
					if !singleFile {
						header := ui.Row(c).FillWidth().Height(24).Padding(0, 10, 0, 6).
							Gap(6).AlignItems(ui.Center).Background(palette.overlay).
							BorderWidth(0, 0, 1, 0).BorderColor(palette.border)
						header.Children(func() {
							ui.Row(c).Grow(1).MinWidth(0).Children(func() {
								ui.Text(c, change.Path).FontSize(codePx(10.5)).FixedLineHeight(codePx(16)).
									Font(monoFont()).FontWeight(500).
									TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
							})
							if change.Additions != nil {
								ui.Text(c, fmt.Sprintf(tr("+%d"), *change.Additions)).FontSize(uiPx(10.5)).
									TextColor(palette.successText)
							}
							if change.Deletions != nil {
								ui.Text(c, fmt.Sprintf("-%d", *change.Deletions)).FontSize(uiPx(10.5)).
									TextColor(palette.dangerText)
							}
						})
					}
					if change.Diff != nil {
						renderUnifiedDiff(c, *change.Diff)
					}
				})
			}
		})
}

// renderUnifiedDiff draws the rows of one patch: 20px bands, a gutter number,
// the sign column, and the code.
func renderUnifiedDiff(c *ui.Context, patch string) {
	type diffLine struct {
		Kind    byte
		Number  int
		Content string
	}
	lines := make([]diffLine, 0, 64)
	oldNumber, newNumber := 0, 0
	for _, raw := range strings.Split(patch, "\n") {
		if strings.HasPrefix(raw, "@@") {
			line := diffLine{Kind: '@', Content: raw}
			lines = append(lines, line)
			if start := hunkStart(raw); start > 0 {
				oldNumber, newNumber = start, hunkNewStart(raw, start)
			}
			continue
		}
		if strings.HasPrefix(raw, "--- ") || strings.HasPrefix(raw, "+++ ") ||
			strings.HasPrefix(raw, "diff ") || strings.HasPrefix(raw, "index ") {
			continue
		}
		switch {
		case strings.HasPrefix(raw, "+"):
			newNumber++
			lines = append(lines, diffLine{Kind: '+', Number: newNumber, Content: raw[1:]})
		case strings.HasPrefix(raw, "-"):
			oldNumber++
			lines = append(lines, diffLine{Kind: '-', Number: oldNumber, Content: raw[1:]})
		default:
			oldNumber++
			newNumber++
			content := raw
			if strings.HasPrefix(content, " ") {
				content = content[1:]
			}
			lines = append(lines, diffLine{Kind: ' ', Number: newNumber, Content: content})
		}
	}
	width := float32(16)
	for _, line := range lines {
		if line.Kind == '@' {
			continue
		}
		if digits := len(fmt.Sprintf("%d", line.Number)); float32(digits)*7.0 > width {
			width = float32(digits) * 7.0
		}
	}
	hidden := len(lines) - activityDiffMaxRows
	if hidden > 0 {
		lines = lines[:activityDiffMaxRows]
	}
	for _, line := range lines {
		if line.Kind == '@' {
			ui.Row(c).FillWidth().Height(20).Background(palette.overlay).
				AlignItems(ui.Center).Children(func() {
				ui.Row(c).Width(width + 28).Shrink(0).Center().Children(func() {
					ui.Text(c, "...").FontSize(codePx(10.5)).FixedLineHeight(codePx(16)).
						Font(monoFont()).TextColor(palette.textMuted)
				})
				ui.Row(c).Grow(1).MinWidth(0).Children(func() {
					ui.Text(c, line.Content).FontSize(codePx(10.5)).FixedLineHeight(codePx(16)).
						Font(monoFont()).TextColor(palette.textMuted).
						SingleLine().Ellipsis("...")
				})
			})
			continue
		}
		background := ui.Transparent
		sign := " "
		signColor := palette.textGhost
		switch line.Kind {
		case '+':
			background = ui.RGBA(palette.success.R, palette.success.G, palette.success.B, 0.08)
			sign = "+"
			signColor = palette.successText
		case '-':
			background = ui.RGBA(palette.danger.R, palette.danger.G, palette.danger.B, 0.08)
			sign = "−"
			signColor = palette.dangerText
		}
		ui.Row(c).FillWidth().MinWidth(0).Height(20).AlignItems(ui.Center).
			Background(background).Children(func() {
			ui.Row(c).Width(width).Shrink(0).Padding(0, 8).Children(func() {
				ui.Text(c, fmt.Sprintf(tr("%d"), line.Number)).FontSize(codePx(10.5)).
					FixedLineHeight(codePx(20)).Font(monoFont()).TextAlign(ui.End).
					TextColor(palette.textTertiary)
			})
			ui.Row(c).Grow(1).MinWidth(0).Children(func() {
				ui.Row(c).Width(10).Shrink(0).Children(func() {
					ui.Text(c, sign).FontSize(codePx(10.5)).FixedLineHeight(codePx(20)).
						Font(monoFont()).TextColor(signColor)
				})
				ui.Row(c).Grow(1).MinWidth(0).Padding(0, 10, 0, 2).Children(func() {
					ui.Text(c, line.Content).FontSize(codePx(10.5)).FixedLineHeight(codePx(20)).
						Font(monoFont()).TextColor(palette.textSecondary).Wrap()
				})
			})
		})
	}
	if hidden > 0 {
		note := fmt.Sprintf("%d more lines · open Review to read the whole change", hidden)
		if hidden == 1 {
			note = "1 more line · open Review to read the whole change"
		}
		ui.Row(c).FillWidth().MinWidth(0).Padding(4, 12).Children(func() {
			ui.Text(c, note).FontSize(uiPx(10.5)).FixedLineHeight(uiPx(16)).
				Font(monoFont()).TextColor(palette.textTertiary).Wrap()
		})
	}
}

// activityDiffMaxRows caps one card's diff, after which Review owns the
// change. From activity_diff.rs MAX_ROWS.
const activityDiffMaxRows = 400

func hunkStart(header string) int {
	index := strings.Index(header, "-")
	if index < 0 {
		return 0
	}
	value := 0
	for _, r := range header[index+1:] {
		if r < '0' || r > '9' {
			break
		}
		value = value*10 + int(r-'0')
	}
	return value
}

func hunkNewStart(header string, fallback int) int {
	index := strings.Index(header, "+")
	if index < 0 {
		return fallback
	}
	value := 0
	for _, r := range header[index+1:] {
		if r < '0' || r > '9' {
			break
		}
		value = value*10 + int(r-'0')
	}
	if value == 0 {
		return fallback
	}
	return value
}

// ── Activity text helpers, from components.rs ──────────────────────────────

func activityHeaderTitle(activities []model.ActivityItem, liveTurn bool, liveReasoningID string) string {
	if liveTurn && len(activities) > 0 {
		last := &activities[len(activities)-1]
		if last.Reasoning != nil {
			return reasoningActivityTitle(last.Reasoning, liveReasoningID != "" && liveReasoningID == last.ID || !last.Complete && !last.Failed)
		}
		return activityDisplayTitle(last)
	}
	return activitySummary(activities)
}

func activitySummary(activities []model.ActivityItem) string {
	type count struct {
		kind  string
		total int
	}
	counts := make([]count, 0, 4)
	for index := range activities {
		kind := activities[index].Kind
		found := false
		for entry := range counts {
			if counts[entry].kind == kind {
				counts[entry].total++
				found = true
				break
			}
		}
		if !found {
			counts = append(counts, count{kind: kind, total: 1})
		}
	}
	parts := make([]string, 0, len(counts))
	for _, entry := range counts {
		singular, plural := activityNoun(entry.kind)
		noun := singular
		if entry.total != 1 {
			noun = plural
		}
		parts = append(parts, fmt.Sprintf(tr(noun), entry.total))
	}
	running := false
	for index := range activities {
		if !activities[index].Complete {
			running = true
			break
		}
	}
	joined := strings.Join(parts, " · ")
	if running {
		return fmt.Sprintf(tr("Running %s"), joined)
	}
	return fmt.Sprintf(tr("Ran %s"), joined)
}

func activityNoun(kind string) (string, string) {
	switch kind {
	case model.KindReasoning:
		return "%d thought", "%d thoughts"
	case model.KindCommand:
		return "%d command", "%d commands"
	case model.KindFileChange:
		return "%d file edit", "%d file edits"
	case model.KindFileRead:
		return "%d file read", "%d file reads"
	case model.KindFileSearch:
		return "%d file search", "%d file searches"
	case model.KindFileList:
		return "%d file listing", "%d file listings"
	case model.KindSearch:
		return "%d search", "%d searches"
	case model.KindPlan:
		return "%d plan step", "%d plan steps"
	default:
		return "%d tool call", "%d tool calls"
	}
}

func activityDisplayTitle(activity *model.ActivityItem) string {
	if activity.Kind == model.KindReasoning {
		if activity.Reasoning != nil {
			return reasoningActivityTitle(activity.Reasoning, !activity.Complete && !activity.Failed)
		}
		return activityActionLabel(activity)
	}
	action := activityActionLabel(activity)
	detail := activityRowDetail(activity, false)
	if detail == "" || detail == action {
		return action
	}
	return action + " " + detail
}

func activityActionLabel(activity *model.ActivityItem) string {
	if activity.Kind == model.KindReasoning {
		return tr("Thinking")
	}
	title := strings.TrimSpace(activity.Title)
	if title == "" {
		return "Tool"
	}
	return title
}

func activityRowDetail(activity *model.ActivityItem, reasoningLive bool) string {
	switch activity.Kind {
	case model.KindReasoning:
		if activity.Reasoning != nil {
			return reasoningActivityTitle(activity.Reasoning, reasoningLive)
		}
		return activity.Title
	case model.KindCommand:
		if activity.DisplayTarget != nil {
			return *activity.DisplayTarget
		}
		if activity.DisplayDescription != nil {
			return *activity.DisplayDescription
		}
		return ""
	case model.KindFileChange:
		switch len(activity.FileChanges) {
		case 0:
			return ""
		case 1:
			return activity.FileChanges[0].DisplayName()
		default:
			return fmt.Sprintf(tr("%d files"), len(activity.FileChanges))
		}
	case model.KindFileRead, model.KindFileList:
		if activity.DisplayTarget != nil {
			return pathBase(*activity.DisplayTarget)
		}
		return ""
	case model.KindFileSearch, model.KindSearch, model.KindPlan:
		if activity.DisplayTarget != nil {
			return *activity.DisplayTarget
		}
		return ""
	case model.KindTool:
		if isAskUserQuestion(activity) {
			return ""
		}
		if activity.DisplayTarget != nil {
			return *activity.DisplayTarget
		}
		return ""
	}
	return ""
}

func reasoningActivityTitle(reasoning *model.ReasoningBlock, live bool) string {
	return reasoningGroupTitle(reasoning, live, 1)
}

func reasoningGroupTitle(reasoning *model.ReasoningBlock, live bool, groupCount int) string {
	base := tr("Thinking")
	if !live {
		base = tr("Thought")
		if reasoning.StartedAtMs > 0 && reasoning.FinishedAtMs > reasoning.StartedAtMs {
			seconds := (reasoning.FinishedAtMs - reasoning.StartedAtMs + 999) / 1000
			base = fmt.Sprintf(tr("Thought for %s"), formatWorkedDuration(seconds))
		}
	}
	if groupCount > 1 {
		return fmt.Sprintf("%s (×%d)", base, groupCount)
	}
	return base
}

func formatWorkedDuration(seconds uint64) string {
	unit := func(value uint64, singular, plural string) string {
		if value == 1 {
			return fmt.Sprintf(tr(singular), value)
		}
		return fmt.Sprintf(tr(plural), value)
	}
	switch {
	case seconds < 60:
		return unit(seconds, "%d second", "%d seconds")
	case seconds < 3600:
		minutes := seconds / 60
		rest := seconds % 60
		if rest == 0 {
			return unit(minutes, "%d minute", "%d minutes")
		}
		return unit(minutes, "%d minute", "%d minutes") + " " + unit(rest, "%d second", "%d seconds")
	default:
		hours := seconds / 3600
		minutes := (seconds % 3600) / 60
		if minutes == 0 {
			return unit(hours, "%d hour", "%d hours")
		}
		return unit(hours, "%d hour", "%d hours") + " " + unit(minutes, "%d minute", "%d minutes")
	}
}

func activityPreview(activity *model.ActivityItem) string {
	detail := ""
	if activity.Detail != nil {
		detail = strings.TrimSpace(*activity.Detail)
	}
	if strings.EqualFold(detail, "failed") && activity.Output != nil {
		for _, line := range strings.Split(*activity.Output, "\n") {
			if strings.TrimSpace(line) != "" {
				return strings.TrimSpace(line)
			}
		}
	}
	if (detail == "" || strings.EqualFold(detail, "failed")) && len(activity.ImageURLs) > 0 {
		return "Image output"
	}
	return detail
}

// activityDisclosureSections splits an activity's raw fields into the blocks
// its expanded card shows.
func activityDisclosureSections(activity *model.ActivityItem) []activitySection {
	sections := make([]activitySection, 0, 3)
	if activity.Kind == model.KindCommand {
		command := ""
		if activity.Arguments != nil {
			command = strings.TrimSpace(*activity.Arguments)
		}
		if command == "" && activity.DisplayTarget != nil {
			command = strings.TrimSpace(*activity.DisplayTarget)
		}
		if command != "" {
			sections = append(sections, activitySection{Kind: sectionCommand, Label: "Command", Content: command})
		}
		output := ""
		if activity.Output != nil {
			output = strings.TrimSpace(*activity.Output)
		}
		if output != "" {
			sections = append(sections, activitySection{Kind: sectionOutput, Label: "Output", Content: output})
		} else if len(activity.ImageURLs) > 0 {
			sections = append(sections, activitySection{Kind: sectionOutput, Label: "Output"})
		}
		return sections
	}
	showsDiff := activityShowsDiff(activity)
	if activity.Arguments != nil && !showsDiff {
		arguments := strings.TrimSpace(*activity.Arguments)
		if arguments != "" {
			sections = append(sections, activitySection{
				Kind:    sectionArguments,
				Label:   "Arguments",
				Content: unwrapCodeModeArguments(arguments),
			})
		}
	}
	output := ""
	if activity.Output != nil {
		output = strings.TrimSpace(*activity.Output)
	}
	if output != "" && (!showsDiff || activity.Failed) {
		sections = append(sections, activitySection{Kind: sectionOutput, Label: "Output", Content: output})
	} else if len(activity.ImageURLs) > 0 {
		sections = append(sections, activitySection{Kind: sectionOutput, Label: "Output"})
	}
	if len(sections) == 0 && activity.Detail != nil {
		detail := strings.TrimSpace(*activity.Detail)
		if detail != "" {
			sections = append(sections, activitySection{Kind: sectionDetail, Content: detail})
		}
	}
	return sections
}

func unwrapCodeModeArguments(arguments string) string {
	var value map[string]json.RawMessage
	if err := json.Unmarshal([]byte(arguments), &value); err != nil {
		return arguments
	}
	if len(value) != 1 {
		return arguments
	}
	raw, ok := value["code"]
	if !ok {
		return arguments
	}
	var code string
	if err := json.Unmarshal(raw, &code); err != nil {
		return arguments
	}
	if strings.TrimSpace(code) == "" {
		return arguments
	}
	return code
}

func activityShowsDiff(activity *model.ActivityItem) bool {
	if activity.Kind != model.KindFileChange {
		return false
	}
	for _, change := range activity.FileChanges {
		if change.Diff != nil {
			return true
		}
	}
	return false
}

func activityFileChangeStats(activity *model.ActivityItem) (uint64, uint64, bool) {
	if activity.Kind != model.KindFileChange || !activity.Complete || activity.Failed ||
		len(activity.FileChanges) == 0 {
		return 0, 0, false
	}
	additions := uint64(0)
	deletions := uint64(0)
	for _, change := range activity.FileChanges {
		if change.Additions == nil || change.Deletions == nil {
			return 0, 0, false
		}
		additions += *change.Additions
		deletions += *change.Deletions
	}
	return additions, deletions, true
}

func isAskUserQuestion(activity *model.ActivityItem) bool {
	if activity.Kind != model.KindTool {
		return false
	}
	leaf := toolNameLeaf(activity.Title)
	var compact strings.Builder
	for _, r := range leaf {
		if r == '_' || r == '-' || r == ' ' {
			continue
		}
		compact.WriteRune(r)
	}
	value := strings.ToLower(compact.String())
	return value == "askuserquestion" || value == "question"
}

func toolNameLeaf(name string) string {
	trimmed := strings.TrimSpace(name)
	leaf := trimmed
	if index := strings.LastIndex(trimmed, "__"); index >= 0 {
		leaf = trimmed[index+2:]
	}
	for _, separator := range []string{":", ".", "/"} {
		if index := strings.LastIndex(leaf, separator); index >= 0 {
			leaf = leaf[index+1:]
		}
	}
	return leaf
}

func pathBase(value string) string {
	last := -1
	for index, r := range value {
		if r == '/' || r == '\\' {
			last = index
		}
	}
	name := value[last+1:]
	if name == "" {
		return value
	}
	return name
}
