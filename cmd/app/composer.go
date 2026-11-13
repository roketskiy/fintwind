package main

import (
	"fmt"
	"strings"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/ui"
)

// Values from composer.rs and ui/mod.rs's MenuChip.
const (
	composerChipHeight  = 28.0
	composerChipInset   = 9.0
	composerChipRadius  = 7.0
	composerChipGap     = 7.0
	composerChipIcon    = 12.0
	composerChipText    = 12.5
	composerModelIcon   = 10.5
	composerInputHeight = 46.0
	composerInputText   = 13.5
	composerInputLine   = 22.0
	composerRowGap      = 4.0
	composerSendSize    = 30.0
)

type composerLabelWidth struct {
	text, font  string
	size, width float32
}

type composerLabels struct {
	model, reasoning, tier, access, interaction bool
}

func (a *app) composerLabelPlan(c *ui.Context) composerLabels {
	plan := composerLabels{true, true, true, true, true}
	width := a.composerCard.Bounds(c).W
	if width <= 0 {
		return plan
	}
	mode := "fullAccess"
	if a.detail != nil {
		switch a.detail.RuntimeMode {
		case "ask", "plan":
			mode = "ask"
		case "autoAcceptEdits":
			mode = "autoAcceptEdits"
		}
	}
	access, _ := accessLabel(mode)
	interaction := tr("Build")
	if a.detail != nil && a.detail.InteractionMode == "plan" {
		interaction = tr("Plan")
	}
	labels := [5]string{"", "", interaction, access, a.modelDisplayName()}
	if model := a.currentProviderModel(); model != nil {
		if len(model.ReasoningEfforts) > 0 {
			labels[0] = reasoningOptionLabel(a.detail, model)
		}
		_, labels[1], _ = modelOptionState(a.detail.ServiceTier, model.DefaultServiceTier, model.ServiceTiers)
	}
	count := 5
	for _, label := range labels[:2] {
		if label == "" {
			count--
		}
	}
	used := float32(20 + 60 + composerRowGap*float32(count+1) + 30*float32(count))
	if a.detail != nil && a.detail.Busy() && (strings.TrimSpace(a.draft) != "" || len(a.pendingAttachments) > 0 || len(a.pendingReferences) > 0) {
		used += composerSendSize + composerRowGap
	}
	widths := [5]float32{}
	for index, label := range labels {
		if label == "" {
			continue
		}
		cached := &a.composerWidths[index]
		size := uiPx(composerChipText)
		if cached.text != label || cached.font != activeAppearance.font || cached.size != size {
			measured, _ := c.MeasureText(0, ui.Span{Text: label, Font: activeAppearance.font, Size: size})
			*cached = composerLabelWidth{text: label, font: activeAppearance.font, size: size, width: measured}
		}
		widths[index] = cached.width + composerChipGap
		if index == 4 {
			widths[index] = min(cached.width, modelChipLabelWidth) + composerChipGap
		}
		used += widths[index]
	}
	for index, show := range []*bool{&plan.reasoning, &plan.tier, &plan.interaction, &plan.access, &plan.model} {
		if used <= width {
			break
		}
		if labels[index] != "" {
			*show = false
			used -= widths[index]
		}
	}
	return plan
}

func (a *app) composer(c *ui.Context) {
	a.refreshAutocompleteSources()
	labels := a.composerLabelPlan(c)
	ui.Column(c).FillWidth().Shrink(0).Padding(0, 20).Children(func() {
		a.composerGoal(c)
		a.capsuleAnchor(c)
		card := ui.Column(c.Key("composer-card")).Bind(&a.composerCard).FillWidth().MaxWidth(transcriptWidth).Margin(0, ui.Auto).
			Background(palette.composer).Border(1, palette.cardBorder).Radius(cardRadius).
			PaddingY(10)
		card = cardShadow(card)
		card.Children(func() {
			if a.detail != nil && len(a.detail.QueuedMessages) > 0 {
				a.queuedMessages(c)
			}
			if len(a.pendingReferences) > 0 || len(a.pendingAttachments) > 0 {
				ui.Row(c).FillWidth().Padding(2, 14, 6, 14).Wrap().Gap(6).Children(func() {
					a.pendingReferenceChips(c)
					a.pendingAttachmentChips(c)
				})
			}
			ui.Row(c).FillWidth().Padding(2, 14, 0, 14).Children(func() {
				// Stable identity before construction: the draft keeps
				// focus and text across rebuilds. IME composing keys never
				// reach OnTextKey (they belong to the input method), so no
				// Composing guard is needed here.
				c.Key("composer-draft")
				if caret, changed := a.composerPaste.Prepare(a.composerTargetSessionID(), &a.draft); changed {
					a.pendingCaret = &caret
				}
				input := ui.TextAreaBase(c, &a.draft).FillWidth().Lines(1, 8).
					MinHeight(composerInputHeight).FixedLineHeight(uiPx(composerInputLine)).
					Placeholder(tr("Do anything…")).
					Label(tr("Message")).FontSize(uiPx(composerInputText)).TextColor(palette.text)
				// Only the first composer frame after a
				// session change takes the keyboard. Later frames must not
				// steal it back from wherever the user clicked. Choosing a
				// model or closing a menu returns focus the same way.
				wantFocus := composerShouldFocus(a)
				if a.focusComposerOnce {
					wantFocus = true
					a.focusComposerOnce = false
				}
				if wantFocus {
					input.Focus()
				}
				if a.pendingCaret != nil {
					input.SetTextSelection(*a.pendingCaret, *a.pendingCaret)
					a.pendingCaret = nil
				}
				start, end := input.TextSelection()
				a.composerCaretStart, a.composerCaretEnd = start, end
				a.composerPaste.Bind(input, &a.draft, c.Services(), a.post)
				input.OnTextKey(func(k ui.TextKey) bool {
					a.composerPaste.TextKey(k)
					// Autocomplete navigation outranks submit: Up/Down move,
					// Enter/Tab accept, Esc dismisses first (the next Esc
					// arms stop when busy).
					if trig := detectAutocompleteTrigger(k.Text(), a.composerCaretEnd); trig != nil && !a.autocompleteDismissed {
						candidates := a.autocompleteRows(trig)
						if len(candidates) > 0 {
							highlight := a.autocompleteHighlight
							if highlight >= len(candidates) {
								highlight = 0
							}
							switch k.Key {
							case ui.KeyUp:
								if highlight <= 0 {
									highlight = len(candidates) - 1
								} else {
									highlight--
								}
								a.autocompleteHighlight = highlight
								return true
							case ui.KeyDown:
								highlight++
								if highlight >= len(candidates) {
									highlight = 0
								}
								a.autocompleteHighlight = highlight
								return true
							case ui.KeyEnter, ui.KeyTab:
								if k.Key == ui.KeyEnter && k.Mods != 0 {
									return false
								}
								row := candidates[highlight]
								insert := ""
								if trig.command {
									insert = "/" + row.name + " "
								} else {
									insert = "@" + row.name + " "
								}
								next, caret := applyAutocompleteInsert(k.Text(), trig, insert)
								a.draft = next
								a.pendingCaret = &caret
								a.autocompleteDismissed = true
								a.autocompleteKey = ""
								if a.detail != nil {
									a.stateForSession(a.detail).draft = next
								}
								k.SetText(next)
								return true
							case ui.KeyEscape:
								a.autocompleteDismissed = true
								return true
							}
						}
					}
					if k.Key == ui.KeyEscape {
						// Busy: first Esc arms stop, second cancels.
						// Idle: unconsumed, default behavior kept.
						return a.pressEscapeStop()
					}
					if k.Key == ui.KeyV && k.Mods == ui.Ctrl {
						// A file clipboard stages uploads instead of text,
						// mirroring the source's paste path; anything else
						// falls through to the normal paste.
						if a.importClipboardFiles() {
							return true
						}
						return false
					}
					if k.Key != ui.KeyEnter || k.Mods != 0 {
						return false
					}
					if strings.TrimSpace(k.Text()) == "" && len(a.pendingAttachments) == 0 && len(a.pendingReferences) == 0 {
						return false
					}
					if a.submitSessionSubmission(sessionSubmission{
						sessionID:   a.composerTargetSessionID(),
						text:        a.promptWithReferences(k.Text()),
						attachments: append([]draftAttachment(nil), a.pendingAttachments...),
					}) {
						a.draft = ""
						a.pendingAttachments = nil
						a.pendingReferences = nil
						a.autocompleteDismissed = false
						a.autocompleteKey = ""
						k.SetText("")
					}
					return true
				})
			})
			a.composerUploadNotice(c)
			ui.Row(c).FillWidth().Padding(8, 10, 0, 10).Gap(composerRowGap).Wrap().
				AlignItems(ui.Center).Children(func() {
				a.modelChip(c, labels.model)
				a.reasoningEffortChip(c, labels.reasoning)
				a.serviceTierChip(c, labels.tier)
				a.accessChip(c, labels.access)
				a.interactionChip(c, labels.interaction)
				a.attachButton(c)
				ui.Row(c).Grow(1)
				busy := a.detail != nil && a.detail.Busy()
				hasDraft := strings.TrimSpace(a.draft) != "" || len(a.pendingAttachments) > 0 || len(a.pendingReferences) > 0
				uploading := a.composerUploading()
				sendable := hasDraft && !uploading
				if busy {
					stop := ui.ButtonBase(c).Label(tr("Stop")).Size(composerSendSize, composerSendSize).
						Radius(15).Center().Background(palette.overlayStrong)
					if stop.Hovered() {
						stop.Background(palette.dangerSoft)
					}
					if stop.Pressed() {
						stop.Opacity(0.8)
					}
					stop.Children(func() {
						if a.escapeStopArmedForCurrent() {
							ui.Text(c, tr("Esc")).FontSize(uiPx(10.5)).FontWeight(600).
								TextColor(palette.text)
						} else {
							icon(c, "stop", 20, palette.text)
						}
					})
					if stop.Clicked() {
						a.disarmEscapeStop()
						a.cancelTurn()
					}
					if hasDraft {
						label := tr("Queue as follow-up")
						if a.runtimePreferences().BusySendSteer && a.detail.Status == "working" && a.runtimes[a.detail.ID] != "" {
							label = tr("Send as steer")
						}
						queue := ui.ButtonBase(c).Label(label).
							Size(composerSendSize, composerSendSize).Radius(15).Center().
							Background(palette.inverse).Disabled(uploading)
						if queue.Hovered() {
							queue.Opacity(0.9)
						}
						if queue.Pressed() {
							queue.Opacity(0.8)
						}
						queue.Children(func() {
							icon(c, "arrow-up", 18, palette.onInverse)
						})
						if queue.Clicked() {
							if a.submitSessionSubmission(sessionSubmission{
								sessionID:   a.composerTargetSessionID(),
								text:        a.promptWithReferences(a.draft),
								attachments: append([]draftAttachment(nil), a.pendingAttachments...),
							}) {
								a.draft = ""
								a.pendingAttachments = nil
								a.pendingReferences = nil
								a.autocompleteDismissed = false
								a.autocompleteKey = ""
							}
						}
					}
					return
				}
				send := ui.ButtonBase(c).Label(tr("Send")).Size(composerSendSize, composerSendSize).
					Radius(15).Center().Disabled(!sendable)
				if sendable {
					send.Background(palette.inverse)
					if send.Hovered() {
						send.Opacity(0.9)
					}
					if send.Pressed() {
						send.Opacity(0.8)
					}
				} else {
					send.Background(palette.overlayStrong)
				}
				send.Children(func() {
					iconColor := palette.textGhost
					if sendable {
						iconColor = palette.onInverse
					}
					icon(c, "arrow-up", 18, iconColor)
				})
				if send.Clicked() && sendable {
					if a.submitSessionSubmission(sessionSubmission{
						sessionID:   a.composerTargetSessionID(),
						text:        a.promptWithReferences(a.draft),
						attachments: append([]draftAttachment(nil), a.pendingAttachments...),
					}) {
						a.draft = ""
						a.pendingAttachments = nil
						a.pendingReferences = nil
						a.autocompleteDismissed = false
						a.autocompleteKey = ""
					}
				}
			})
		})
		// Autocomplete floats above the card like render_composer_autocomplete:
		// an anchored overlay at card width, never pushing the layout.
		if trigger, rows := a.currentAutocompleteTrigger(); trigger != nil && len(rows) > 0 {
			a.autocompleteOpen = true
			winW, _ := c.Size()
			panelW := transcriptWidth
			if avail := winW - 40; avail < panelW {
				panelW = avail
			}
			if panelW < 200 {
				panelW = 200
			}
			ui.PopoverBase(c, card, &a.autocompleteOpen, func(panel ui.Element) {
				panel.Background(palette.raised).Border(1, palette.borderStrong).Radius(11).
					Padding(0).Width(panelW).
					Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.28)).
					Shadow(0, 12, 28, 0, ui.RGBA(0, 0, 0, 0.22)).
					Children(func() {
						a.renderAutocompleteList(c, trigger, rows)
					})
			})
			if !a.autocompleteOpen {
				// Closed externally (click-out): keep this trigger shut.
				a.autocompleteDismissed = true
			}
		} else {
			a.autocompleteOpen = false
		}
		a.handleComposerDrop(card)
	})
}

func (a *app) queuedMessages(c *ui.Context) {
	session := a.detail
	if session == nil {
		return
	}
	collapsed := a.queuedCollapsed(session.ID)
	var dragged queuedMessageDrag
	var targetID string
	ui.Column(c).FillWidth().Padding(0, 10, 8, 10).Children(func() {
		ui.Column(c).FillWidth().Padding(4).Radius(8).
			Background(palette.raised).Border(1, palette.cardBorder).Children(func() {
			header := ui.ButtonBase(c).Label(tr("Queued")).Height(28).Padding(0, 8).
				Radius(6).Gap(6).AlignItems(ui.Center)
			if header.Hovered() {
				header.Background(palette.overlay)
			}
			header.Children(func() {
				icon(c, "queue", 14, palette.textSecondary)
				ui.Text(c, fmt.Sprintf(tr("Queued · %d"), len(session.QueuedMessages))).
					FontSize(uiPx(12)).TextColor(palette.textSecondary)
				chevron := "chevron-up"
				if collapsed {
					chevron = "chevron-down"
				}
				icon(c, chevron, 11, palette.textTertiary)
			})
			if header.Clicked() {
				a.setQueuedCollapsed(session.ID, !collapsed)
			}
			if collapsed {
				return
			}
			ui.Scroll(c).FillWidth().MaxHeight(160).TrackScroll(&a.stateForSession(session).queueScroll).Children(func() {
				ui.Column(c).FillWidth().Padding(5, 0, 0, 0).Gap(4).Children(func() {
					for index := len(session.QueuedMessages) - 1; index >= 0; index-- {
						message := session.QueuedMessages[index]
						// The Steer control only shows while the turn can take it,
						// mirroring the steerable gate in render_queued_messages.
						steerable := a.queuedSteerable(session)
						row := ui.Row(c.Key("queued-"+message.ID)).FillWidth().Padding(6, 8).Radius(6).
							Gap(8).AlignItems(ui.Center).Focusable().TabStop(true).
							Label(tr("Reorder queued message")).Description(tr("Drag to reorder; Ctrl+Up/Down to move")).
							Cursor(ui.CursorGrab).Drag(queuedMessageDrag{sessionID: session.ID, messageID: message.ID}).
							OnShortcut(ui.Ctrl, ui.KeyUp, func() { a.moveQueuedMessage(session.ID, message.ID, 1) }).
							OnShortcut(ui.Ctrl, ui.KeyDown, func() { a.moveQueuedMessage(session.ID, message.ID, -1) })
						if over, ok := ui.DragOver[queuedMessageDrag](row); ok && over.sessionID == session.ID && over.messageID != message.ID {
							row.Background(palette.overlayStrong)
						} else if row.Hovered() {
							row.Background(palette.overlay)
						}
						if value, ok := ui.Drop[queuedMessageDrag](row); ok {
							dragged, targetID = value, message.ID
						}
						row.Children(func() {
							icon(c, "grip-vertical", 13, palette.textTertiary)
							ui.Column(c).Grow(1).MinWidth(0).Gap(4).Children(func() {
								if strings.TrimSpace(message.Content) != "" || message.DisplayContent != nil && strings.TrimSpace(*message.DisplayContent) != "" {
									ui.Text(c, queuedContent(message)).FontSize(uiPx(13)).
										TextColor(palette.text).SingleLine().Ellipsis("...")
								}
								if len(message.Attachments) > 0 {
									ui.Row(c).FillWidth().Wrap().Gap(4).Children(func() {
										for _, attachment := range message.Attachments {
											ui.Row(c).MaxWidth(200).Padding(2, 6).Radius(5).Gap(4).AlignItems(ui.Center).Background(palette.overlay).Tooltip(attachment.Path).Children(func() {
												if attachment.IsDir {
													icon(c, "folder", 12, palette.textSecondary)
												} else if attachment.IsImage {
													icon(c, "modality-image", 12, palette.textSecondary)
												} else {
													brandIcon(c, fileTypeIcon(attachment.Name), 12)
												}
												ui.Text(c, attachment.Name).FontSize(uiPx(11)).TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
											})
										}
									})
								}
							})
							ui.Row(c).Gap(2).AlignItems(ui.Center).Children(func() {
								if steerable {
									steer := ui.ButtonBase(c).Label(tr("Steer")).
										Height(22).Padding(0, 8).Radius(5).Center().
										Border(1, palette.border)
									if steer.Hovered() {
										steer.Background(palette.overlay)
									}
									if steer.Pressed() {
										steer.Background(palette.overlayStrong)
									}
									steer.Children(func() {
										ui.Text(c, tr("Steer")).FontSize(uiPx(11.5)).FontWeight(500).
											TextColor(palette.text)
									})
									if steer.Clicked() {
										a.steerQueuedMessage(session.ID, message.ID)
									}
								}
								edit := ui.ButtonBase(c).Label(tr("Edit in composer")).
									Size(22, 22).
									Radius(5).Center()
								if edit.Hovered() {
									edit.Background(palette.overlayStrong)
								}
								edit.Children(func() {
									icon(c, "pencil", 13, palette.textTertiary)
								})
								if edit.Clicked() {
									a.editQueuedMessage(session.ID, message.ID)
								}
								remove := ui.ButtonBase(c).Label(tr("Remove from queue")).
									Size(22, 22).
									Radius(5).Center()
								if remove.Hovered() {
									remove.Background(palette.overlayStrong)
								}
								remove.Children(func() {
									icon(c, "trash", 13, palette.textTertiary)
								})
								if remove.Clicked() {
									a.removeQueuedMessage(session.ID, message.ID)
								}
							})
						})
					}
				})
			})
		})
	})
	if dragged.sessionID == session.ID && targetID != "" {
		a.reorderQueuedMessage(session.ID, dragged.messageID, targetID)
	}
}

func queuedContent(message model.QueuedMessage) string {
	if message.DisplayContent != nil && strings.TrimSpace(*message.DisplayContent) != "" {
		return *message.DisplayContent
	}
	if strings.TrimSpace(message.Content) != "" {
		return message.Content
	}
	names := make([]string, 0, len(message.Attachments))
	for _, attachment := range message.Attachments {
		names = append(names, attachment.Name)
	}
	return strings.Join(names, ", ")
}

// permissionCard is the provider's approval request, above the composer.
func (a *app) permissionCard(c *ui.Context) {
	if a.pendingUserInput != nil {
		a.userInputCard(c)
		return
	}
	pending := a.pendingPermission
	if pending == nil {
		return
	}
	ui.Row(c).FillWidth().Padding(0, 20, 8, 20).Children(func() {
		card := ui.Column(c).FillWidth().MaxWidth(transcriptWidth).Margin(0, ui.Auto).
			Padding(12).Radius(12).Border(1, palette.borderStrong).
			Background(palette.raised)
		card = cardShadow(card)
		card.Children(func() {
			ui.Row(c).Gap(8).AlignItems(ui.Center).Children(func() {
				icon(c, "alert", 13, palette.warning)
				ui.Text(c, pending.Title).FontSize(uiPx(12.5)).FontWeight(500).TextColor(palette.text)
			})
			detail := ui.Row(c).FillWidth().Margin(8, 0, 0, 0).MaxHeight(92).
				Padding(8).Radius(7).Background(palette.inset)
			detail.Children(func() {
				ui.Column(c).FillWidth().Children(func() {
					ui.Text(c, pending.Detail).FontSize(uiPx(10.5)).FixedLineHeight(uiPx(16)).
						Font(monoFont()).TextColor(palette.textSecondary).Wrap()
				})
			})
			buttons := ui.Row(c).Margin(10, 0, 0, 0).Gap(8).AlignItems(ui.Center)
			buttons.Children(func() {
				for _, option := range pending.Options {
					option := option
					button := ui.ButtonBase(c).Label(option.Label).Height(32).
						Padding(0, 14).Radius(8).Center().Disabled(pending.Response.Phase != "")
					if option.Allow {
						button.Background(palette.inverse)
						if button.Hovered() || button.Pressed() {
							button.Opacity(0.9)
						}
						button.Children(func() {
							ui.Text(c, option.Label).FontSize(uiPx(12.5)).FontWeight(600).
								TextColor(palette.onInverse)
						})
					} else {
						button.Border(1, palette.borderStrong)
						if button.Hovered() {
							button.Background(palette.overlay)
						}
						if button.Pressed() {
							button.Background(palette.overlayStrong)
						}
						button.Children(func() {
							ui.Text(c, option.Label).FontSize(uiPx(12.5)).FontWeight(600).
								TextColor(palette.textSecondary)
						})
					}
					if button.Clicked() {
						a.respondPermission(option.ID)
					}
				}
			})
			sessionResponseNotice(c, pending.Response)
		})
	})
}

// userInputCard is the agent's question: a header with progress, the question,
// its options, a free-form answer field, and the footer controls.
func (a *app) userInputCard(c *ui.Context) {
	pending := a.pendingUserInput
	if pending == nil || pending.Index < 0 || pending.Index >= len(pending.Questions) {
		return
	}
	question := pending.Questions[pending.Index]
	selected := pending.Selections[pending.Index]
	hasSelected := func(label string) bool {
		for _, value := range selected {
			if value == label {
				return true
			}
		}
		return false
	}
	custom := pending.Custom[pending.Index]
	canContinue := strings.TrimSpace(custom) != "" || len(selected) > 0
	isLast := pending.Index+1 == len(pending.Questions)

	ui.Row(c).FillWidth().Padding(0, 20, 8, 20).Children(func() {
		card := ui.Column(c).FillWidth().MaxWidth(transcriptWidth).Margin(0, ui.Auto).
			Padding(12, 14, 10, 14).Radius(13).Border(1, palette.border).
			Background(palette.composer)
		card.Children(func() {
			header := ui.Row(c).FillWidth().PaddingY(2).Gap(8).AlignItems(ui.Center)
			header.Children(func() {
				ui.Text(c, question.Header).FontSize(uiPx(10.5)).FontWeight(600).
					TextColor(palette.textTertiary).Shrink(0)
				if len(pending.Questions) > 1 {
					ui.Row(c).Height(18).Padding(0, 6).Radius(5).
						Background(palette.overlay).Center().Children(func() {
						ui.Text(c, fmt.Sprintf(tr("%d of %d"), pending.Index+1,
							len(pending.Questions))).FontSize(uiPx(9.5)).FontWeight(500).
							TextColor(palette.textTertiary)
					})
				}
				ui.Row(c).Grow(1)
			})
			if pending.Collapsed {
				ui.Row(c).Height(8)
			} else {
				ui.Text(c, question.Question).FontSize(uiPx(13)).FixedLineHeight(uiPx(18)).
					FontWeight(500).TextColor(palette.text).Wrap().Margin(5, 0, 0, 0)
				if len(question.Options) > 0 {
					options := ui.Column(c).FillWidth().Margin(9, 0, 0, 0).Gap(4)
					options.Children(func() {
						for _, option := range question.Options {
							option := option
							isSelected := hasSelected(option.Label)
							row := ui.ButtonBase(c).Label(option.Label).FillWidth().Disabled(pending.Response.Phase != "").
								MinHeight(38).Padding(6, 10).Radius(8).Gap(8).
								Justify(ui.Start).AlignItems(ui.Center)
							if isSelected {
								row.Background(alpha(palette.accent, 0.08))
								row.Border(1, alpha(palette.accent, 0.34))
							} else {
								row.Background(palette.overlay)
								row.Border(1, ui.Transparent)
								if row.Hovered() {
									row.Background(palette.overlayStrong)
									row.Border(1, palette.border)
								}
							}
							row.Children(func() {
								ui.Column(c).Grow(1).MinWidth(0).Children(func() {
									ui.Text(c, option.Label).FontSize(uiPx(12.5)).FontWeight(500).
										TextColor(palette.text)
									if option.Description != nil && *option.Description != "" {
										ui.Text(c, *option.Description).FontSize(uiPx(11)).
											FixedLineHeight(uiPx(14)).TextColor(palette.textSecondary).
											Wrap().Margin(1, 0, 0, 0)
									}
								})
								if isSelected {
									icon(c, "check", 13, palette.accent)
								}
							})
							if row.Clicked() {
								if question.MultiSelect {
									if isSelected {
										next := make([]string, 0, len(selected))
										for _, value := range selected {
											if value != option.Label {
												next = append(next, value)
											}
										}
										pending.Selections[pending.Index] = next
									} else {
										pending.Selections[pending.Index] = append(selected, option.Label)
									}
								} else {
									pending.Selections[pending.Index] = []string{option.Label}
								}
							}
						}
					})
				}
				field := ui.Row(c).FillWidth().Margin(choice(len(question.Options) == 0, float32(9), float32(4)), 0, 0, 0).
					Height(36).Padding(0, 10).Radius(8).Gap(7).AlignItems(ui.Center)
				if strings.TrimSpace(custom) != "" {
					field.Background(alpha(palette.accent, 0.06))
					field.Border(1, alpha(palette.accent, 0.34))
				} else {
					field.Background(palette.overlay)
					field.Border(1, ui.Transparent)
				}
				field.Children(func() {
					color := palette.textGhost
					if strings.TrimSpace(custom) != "" {
						color = palette.accent
					}
					icon(c, "pencil", 12, color)
					ui.Row(c).Grow(1).MinWidth(0).Children(func() {
						ui.TextInputBase(c, &pending.Custom[pending.Index]).FillWidth().
							FontSize(uiPx(12.5)).FixedLineHeight(uiPx(17)).TextColor(palette.text).
							Placeholder(tr("Type another answer")).Label(tr("Another answer")).Disabled(pending.Response.Phase != "")
					})
				})
			}
			footer := ui.Row(c).FillWidth().Margin(8, 0, 0, 0).Gap(4).AlignItems(ui.Center).
				Disabled(pending.Response.Phase != "")
			footer.Children(func() {
				a.userInputTextButton(c, "Skip", func() { a.skipUserInput() })
				fold := "Collapse"
				if pending.Collapsed {
					fold = "Expand"
				}
				a.userInputTextButton(c, fold, func() {
					pending.Collapsed = !pending.Collapsed
				})
				ui.Row(c).Grow(1)
				if pending.Collapsed {
					return
				}
				if pending.Index > 0 {
					a.userInputTextButton(c, "Back", func() {
						pending.Index--
					})
				}
				label := tr("Next")
				if isLast {
					label = tr("Submit")
				}
				button := ui.ButtonBase(c).Label(label).Height(30).Padding(0, 11).
					Radius(7).Center().Disabled(!canContinue || pending.Response.Phase != "")
				if canContinue {
					button.Background(palette.inverse)
					if button.Hovered() || button.Pressed() {
						button.Opacity(0.9)
					}
					button.Children(func() {
						ui.Text(c, label).FontSize(uiPx(12)).FontWeight(600).
							TextColor(palette.onInverse)
					})
					if button.Clicked() {
						if isLast {
							a.submitUserInput()
						} else {
							pending.Index++
						}
					}
				} else {
					button.Background(palette.overlay)
					button.Children(func() {
						ui.Text(c, label).FontSize(uiPx(12)).FontWeight(600).
							TextColor(palette.textGhost)
					})
				}
			})
			sessionResponseNotice(c, pending.Response)
		})
	})
}

func (a *app) userInputTextButton(c *ui.Context, label string, onClick func()) {
	button := ui.ButtonBase(c).Label(label).Height(30).Padding(0, 9).Radius(7).Center()
	if button.Hovered() {
		button.Background(palette.overlay)
	}
	if button.Pressed() {
		button.Opacity(0.8)
	}
	button.Children(func() {
		ui.Text(c, label).FontSize(uiPx(12)).FontWeight(500).TextColor(palette.textTertiary)
	})
	if button.Clicked() {
		onClick()
	}
}

func (a *app) submitUserInput() {
	pending := a.pendingUserInput
	if pending == nil || a.detail == nil || a.client == nil {
		return
	}
	answers := make([]map[string]any, 0, len(pending.Questions))
	for index, question := range pending.Questions {
		values := append([]string{}, pending.Selections[index]...)
		if custom := strings.TrimSpace(pending.Custom[index]); custom != "" {
			values = append(values, custom)
		}
		if len(values) == 0 {
			pending.Index = index
			return
		}
		answers = append(answers, map[string]any{
			"questionId": question.ID,
			"answers":    values,
		})
	}
	sessionID := a.detail.ID
	runtimeID := a.runtimes[sessionID]
	requestID := pending.RequestID
	a.sendSessionReply(sessionID, runtimeID, &pending.Response, map[string]any{
		"type": "respondUserInput", "requestId": requestID, "answers": answers,
	}, func() { a.clearSessionQuestion(sessionID, pending) })
}

func (a *app) skipUserInput() {
	pending := a.pendingUserInput
	if pending == nil || a.detail == nil || a.client == nil {
		return
	}
	sessionID := a.detail.ID
	runtimeID := a.runtimes[sessionID]
	requestID := pending.RequestID
	a.sendSessionReply(sessionID, runtimeID, &pending.Response, map[string]any{
		"type": "cancelUserInput", "requestId": requestID,
	}, func() { a.clearSessionQuestion(sessionID, pending) })
}

func (a *app) respondPermission(optionID string) {
	pending := a.pendingPermission
	if pending == nil || a.detail == nil || a.client == nil {
		return
	}
	sessionID := a.detail.ID
	runtimeID := a.runtimes[sessionID]
	requestID := pending.RequestID
	valid := false
	for _, option := range pending.Options {
		valid = valid || option.ID == optionID
	}
	if !valid {
		return
	}
	a.sendSessionReply(sessionID, runtimeID, &pending.Response, map[string]any{
		"type": "respond", "requestId": requestID, "optionId": optionID,
	}, func() { a.clearSessionPermission(sessionID, pending) })
}

// composerChip draws one MenuChip: 28 high, 9 insets, 7 gap, 12.5 text.
func (a *app) composerChip(c *ui.Context, iconName string, iconColor ui.Color, label string, textColor ui.Color, caret bool) ui.Element {
	chip := ui.ButtonBase(c).Label(label).Height(composerChipHeight).
		Padding(0, composerChipInset).Radius(composerChipRadius).
		Gap(composerChipGap).AlignItems(ui.Center)
	if chip.Hovered() {
		chip.Background(palette.overlay)
	}
	if chip.Pressed() {
		chip.Background(palette.overlayStrong)
	}
	chip.Children(func() {
		if iconName != "" {
			icon(c, iconName, composerChipIcon, iconColor)
		}
		ui.Row(c).MinWidth(0).Children(func() {
			ui.Text(c, label).FontSize(uiPx(composerChipText)).FixedLineHeight(uiPx(16)).
				TextColor(textColor).SingleLine().Ellipsis("...")
		})
		if caret {
			icon(c, "chevron-down", 10, palette.textGhost)
		}
	})
	return chip
}

func (a *app) modelChip(c *ui.Context, showLabel bool) {
	// The chip shows the catalog name, not the raw id, so make sure the
	// catalog is on its way the first time a session renders. The request is
	// idempotent, throttled on failure, and never blocks the frame.
	a.requestModelProbe()
	label := a.modelDisplayName()
	mark := ""
	if a.detail != nil && a.detail.Model != nil {
		mark = companyIconFor(*a.detail.Model)
	}
	if mark == "" {
		mark = companyIconFor(label)
	}
	name := "bot"
	if mark != "" {
		name = mark
	}
	// MenuChip geometry: the model mark draws at 10.5; open reads overlay.
	chip := ui.ButtonBase(c).Label(label).Height(composerChipHeight).
		Padding(0, composerChipInset).Radius(composerChipRadius).
		Gap(composerChipGap).AlignItems(ui.Center).Disabled(a.detail == nil)
	if a.modelMenuOpen {
		chip.Background(palette.overlay)
	}
	if chip.Hovered() {
		chip.Background(palette.overlay)
	}
	if chip.Pressed() {
		chip.Background(palette.overlayStrong)
	}
	chip.Children(func() {
		markIcon(c, name, composerModelIcon, palette.neutral)
		if showLabel {
			ui.Row(c).MinWidth(0).MaxWidth(modelChipLabelWidth).Children(func() {
				ui.Text(c, label).FontSize(uiPx(composerChipText)).FixedLineHeight(uiPx(16)).
					TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
			})
		}
	})
	if chip.Clicked() {
		a.modelMenuOpen = !a.modelMenuOpen
		if a.modelMenuOpen {
			a.modelQuery = ""
			a.modelHighlight = 0
			a.modelMenuJustOpened = true
			a.requestModelProbe()
		}
	}
	// Panel geometry from the picker's root div in
	// render_provider_model_control: w460 r13, border_strong on raised,
	// shadow_lg. PopoverBase flips above the chip on its own since the
	// composer sits at the bottom of the window.
	ui.PopoverBase(c, chip, &a.modelMenuOpen, func(panel ui.Element) {
		panel.Background(palette.raised).Border(1, palette.borderStrong).Radius(13).
			Padding(0).Width(modelPickerWidth).Height(modelPickerHeight).Clip().
			Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.28)).
			Shadow(0, 12, 28, 0, ui.RGBA(0, 0, 0, 0.22)).
			Children(func() {
				a.modelPickerMenu(c)
			})
	})
}

// accessChip mirrors RuntimeMode::access and its labels, and opens the three
// access levels in a menu above the chip.
func (a *app) accessChip(c *ui.Context, showLabel bool) {
	mode := "fullAccess"
	if a.detail != nil {
		switch a.detail.RuntimeMode {
		case "plan", "ask":
			mode = "ask"
		case "autoAcceptEdits":
			mode = "autoAcceptEdits"
		}
	}
	label, iconName := accessLabel(mode)
	chip := ui.ButtonBase(c).Label(label).Description(tr(accessDescription(mode))).Height(composerChipHeight).
		Padding(0, composerChipInset).Radius(composerChipRadius).
		Gap(composerChipGap).AlignItems(ui.Center).Disabled(a.detail == nil)
	if a.accessMenuOpen {
		chip.Background(palette.overlay)
	}
	if chip.Hovered() {
		chip.Background(palette.overlay)
	}
	if chip.Pressed() {
		chip.Background(palette.overlayStrong)
	}
	chip.Children(func() {
		icon(c, iconName, composerChipIcon, palette.textTertiary)
		if showLabel {
			ui.Text(c, label).FontSize(uiPx(composerChipText)).FixedLineHeight(uiPx(16)).
				TextColor(palette.textSecondary)
		}
	})
	if chip.Clicked() {
		a.accessMenuOpen = !a.accessMenuOpen
	}
	ui.PopoverBase(c, chip, &a.accessMenuOpen, func(panel ui.Element) {
		panel.Background(palette.raised).Border(1, palette.borderStrong).Radius(10).
			Padding(4).Width(288).
			Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.28)).
			Shadow(0, 12, 28, 0, ui.RGBA(0, 0, 0, 0.22)).
			Children(func() {
				for _, option := range []string{"ask", "autoAcceptEdits", "fullAccess"} {
					option := option
					optionLabel, optionIcon := accessLabel(option)
					selected := option == mode
					row := ui.ButtonBase(c).Label(optionLabel).FillWidth().Padding(5, 8).
						Radius(7).Gap(10).Justify(ui.Start).AlignItems(ui.Center)
					if row.Hovered() {
						row.Background(palette.overlay)
					}
					row.Children(func() {
						icon(c, optionIcon, 15, palette.textTertiary)
						ui.Column(c).Grow(1).MinWidth(0).Children(func() {
							weight := 500
							if selected {
								weight = 600
							}
							ui.Text(c, optionLabel).FontSize(uiPx(12.5)).FontWeight(weight).
								TextColor(palette.text).SingleLine().Ellipsis("...")
							ui.Text(c, accessDescription(option)).FontSize(uiPx(11)).
								FixedLineHeight(uiPx(15)).TextColor(palette.textTertiary)
						})
						if selected {
							icon(c, "check", 12, palette.textTertiary)
						}
					})
					if row.Clicked() {
						a.accessMenuOpen = false
						a.setRuntimeMode(option)
					}
				}
			})
	})
}

func accessLabel(mode string) (string, string) {
	switch mode {
	case "ask":
		return tr("Supervised"), "lock"
	case "autoAcceptEdits":
		return tr("Auto-accept edits"), "pencil"
	default:
		return tr("Full access"), "lock-open"
	}
}

func accessDescription(mode string) string {
	switch mode {
	case "ask":
		return "Ask before commands and file changes"
	case "autoAcceptEdits":
		return "Auto-approve edits, ask before other actions"
	default:
		return "Allow commands and edits without prompts"
	}
}

// setRuntimeMode updates the draft's access level and the live runtime when
// one exists, so the next provider request follows the choice.
func (a *app) setRuntimeMode(mode string) {
	if a.detail == nil {
		return
	}
	a.detail.RuntimeMode = mode
	a.applySessionOptions()
}

// interactionChip is the build/plan toggle: no caret, accent while planning.
func (a *app) interactionChip(c *ui.Context, showLabel bool) {
	plan := a.detail != nil && a.detail.InteractionMode == "plan"
	iconName := "wrench"
	iconColor := palette.textTertiary
	textColor := palette.textSecondary
	label := tr("Build")
	description := tr("Build: run commands and edits")
	if plan {
		label = tr("Plan")
		iconName = "list"
		iconColor = palette.accent
		textColor = palette.accent
		description = tr("Plan: propose first, apply later")
	}
	chip := ui.ButtonBase(c).Label(label).Description(description).Height(composerChipHeight).
		Padding(0, composerChipInset).Radius(composerChipRadius).
		Gap(composerChipGap).AlignItems(ui.Center).Disabled(a.detail == nil)
	if chip.Hovered() {
		chip.Background(palette.overlay)
	}
	if chip.Pressed() {
		chip.Background(palette.overlayStrong)
	}
	chip.Children(func() {
		icon(c, iconName, composerChipIcon, iconColor)
		if showLabel {
			ui.Text(c, label).FontSize(uiPx(composerChipText)).FixedLineHeight(uiPx(16)).
				TextColor(textColor)
		}
	})
	if chip.Clicked() && a.detail != nil {
		if plan {
			a.detail.InteractionMode = "build"
		} else {
			a.detail.InteractionMode = "plan"
		}
		a.applySessionOptions()
	}
}

// applySessionOptions pushes the draft's current mode choice to the live
// runtime so the next provider request uses it.
func (a *app) applySessionOptions() {
	if a.detail == nil {
		return
	}
	if a.sessionActionBlocksInput(a.detail.ID) {
		return
	}
	a.syncSummary(a.detail)
	a.saveSessionSnapshot(a.detail)
	if a.client == nil || a.runtimes[a.detail.ID] == "" || a.lifetime == nil {
		return
	}
	if a.optionsSync == nil {
		a.optionsSync = make(map[string]*sessionOptionsSync)
	}
	state := a.optionsSync[a.detail.ID]
	if state == nil {
		state = &sessionOptionsSync{}
		a.optionsSync[a.detail.ID] = state
	}
	state.generation++
	if !state.sending {
		a.sendSessionOptions(a.detail, state)
	}
}
