use crate::theme::ui_px;

use super::*;

use chrono::{Datelike, Days};
use gpui::StyledText;
use std::path::Path;

pub(super) fn pulse_dot(size: f32, color: Hsla) -> AnyElement {
    motion::pulse(Duration::from_millis(1600), move |phase| {
        div()
            .w(px(size))
            .h(px(size))
            .flex_none()
            .rounded_full()
            .bg(color)
            .opacity(pulsating_between(0.3, 1.0)(phase))
            .into_any_element()
    })
    // Mounted for whole activities; its pane must not tick at full rate.
    .every(2)
    .into_any_element()
}

/// A grayscale highlight travels through a single shaped text element. Keep
/// the run font identical to the static label so status changes do not reflow.
pub(super) fn flowing_activity_label(
    text: &str,
    phase: f32,
    base: Hsla,
    is_dark: bool,
    weight: FontWeight,
) -> StyledText {
    let highlight: Hsla = if is_dark {
        rgb(0xffffff).into()
    } else {
        rgb(0x050505).into()
    };
    let last = text.chars().count().saturating_sub(1).max(1) as f32;
    let mut label_font = font(crate::theme::ui_font_family());
    label_font.weight = weight;
    let runs = text
        .chars()
        .enumerate()
        .map(|(index, ch)| {
            let position = index as f32 / last;
            let center = phase * 1.6 - 0.3;
            // The plateau keeps a complete glyph visibly highlighted.
            let glow = ((0.34 - (position - center).abs()) / 0.22).clamp(0.0, 1.0);
            let glow = glow * glow * (3.0 - 2.0 * glow);
            TextRun {
                len: ch.len_utf8(),
                font: label_font.clone(),
                color: base.blend(highlight.opacity(glow)),
                background_color: None,
                underline: None,
                strikethrough: None,
            }
        })
        .collect();
    StyledText::new(text).with_runs(runs)
}

/// Three dots chasing a brightness wave, the transcript's "still working"
/// signal. Each dot rides the shared pulse clock with a phase offset, so the
/// bright spot travels left to right. Under reduce-motion the clock holds the
/// cycle's first frame — the lead dot bright, the tail dim — which reads as a
/// static ellipsis.
pub(super) fn working_wave_dots(color: Hsla) -> AnyElement {
    const DOT_PHASE_STEP: f32 = 0.18;
    motion::pulse(Duration::from_millis(1400), move |phase| {
        div()
            .flex()
            .items_center()
            .gap(px(3.5))
            .children((0..3).map(|index| {
                let dot_phase = (phase + 1.0 - index as f32 * DOT_PHASE_STEP) % 1.0;
                let wave = ((dot_phase * std::f32::consts::TAU).sin() + 1.0) / 2.0;
                div()
                    .size(px(4.5))
                    .flex_none()
                    .rounded_full()
                    .bg(color)
                    .opacity(0.25 + 0.75 * wave)
            }))
            .into_any_element()
    })
    // Mounted for the whole turn: this is what sets the transcript pane's
    // tick floor, and every tick rebuilds each visible row. The 1400 ms wave
    // reads identically at half cadence.
    .every(2)
    .into_any_element()
}

pub(super) fn format_message_time(created_at: u64) -> String {
    format_message_time_at(created_at, Local::now())
}

fn format_message_time_at(created_at: u64, now: DateTime<Local>) -> String {
    let Ok(seconds) = i64::try_from(created_at) else {
        return String::new();
    };
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|timestamp| {
            let timestamp = timestamp.with_timezone(&Local);
            let message_date = timestamp.date_naive();
            let today = now.date_naive();
            if crate::i18n::uses_east_asian_date_format() {
                let time = timestamp.format("%H:%M").to_string();
                if message_date >= today {
                    return time;
                }
                if today.pred_opt() == Some(message_date) {
                    return tr!("time.yesterday_at", time = time);
                }
                let week_start = today
                    .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
                    .unwrap_or(today);
                if message_date >= week_start {
                    let weekday = match timestamp.weekday() {
                        chrono::Weekday::Mon => tr!("time.monday"),
                        chrono::Weekday::Tue => tr!("time.tuesday"),
                        chrono::Weekday::Wed => tr!("time.wednesday"),
                        chrono::Weekday::Thu => tr!("time.thursday"),
                        chrono::Weekday::Fri => tr!("time.friday"),
                        chrono::Weekday::Sat => tr!("time.saturday"),
                        chrono::Weekday::Sun => tr!("time.sunday"),
                    };
                    return tr!("time.weekday_at", weekday = weekday, time = time);
                }
                if message_date.year() == today.year() {
                    return tr!(
                        "time.date_at",
                        month = timestamp.month(),
                        day = timestamp.day(),
                        time = time
                    );
                }
                return tr!(
                    "time.full_date_at",
                    year = timestamp.year(),
                    month = timestamp.month(),
                    day = timestamp.day(),
                    time = time
                );
            }
            let time = timestamp
                .format("%I:%M %p")
                .to_string()
                .trim_start_matches('0')
                .to_owned();

            if message_date >= today {
                return time;
            }

            if today.pred_opt() == Some(message_date) {
                return tr!("time.yesterday_at", time = time);
            }

            let week_start = today
                .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
                .unwrap_or(today);
            if message_date >= week_start {
                return format!("{} {time}", timestamp.format("%A"));
            }

            let day = timestamp.day();
            let ordinal_suffix = match day % 100 {
                11..=13 => "th",
                _ => match day % 10 {
                    1 => "st",
                    2 => "nd",
                    3 => "rd",
                    _ => "th",
                },
            };
            let date = if message_date.year() == today.year() {
                format!("{} {day}{ordinal_suffix}", timestamp.format("%b"))
            } else {
                format!(
                    "{} {day}{ordinal_suffix} {}",
                    timestamp.format("%b"),
                    timestamp.year()
                )
            };
            format!("{date}, {time}")
        })
        .unwrap_or_default()
}

impl Fintwind {
    pub(super) fn control_was_copied(&self, control_id: &str) -> bool {
        self.copied_control_feedback.contains_key(control_id)
    }

    pub(super) fn show_control_copied(
        &mut self,
        control_id: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let control_id = control_id.into();
        self.copied_control_generation = self.copied_control_generation.wrapping_add(1);
        let generation = self.copied_control_generation;
        self.copied_control_feedback
            .insert(control_id.clone(), generation);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied_control_feedback.get(&control_id) == Some(&generation) {
                    this.copied_control_feedback.remove(&control_id);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn show_message_copied(&mut self, message_id: Uuid, cx: &mut Context<Self>) {
        self.copied_message_generation = self.copied_message_generation.wrapping_add(1);
        let generation = self.copied_message_generation;
        self.copied_message_feedback.insert(message_id, generation);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied_message_feedback.get(&message_id) == Some(&generation) {
                    this.copied_message_feedback.remove(&message_id);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[allow(clippy::too_many_arguments)]
fn render_message_footer(
    theme: &Theme,
    message: &Message,
    footer_time: u64,
    copy_content: SharedString,
    copied: bool,
    group_name: SharedString,
    align_right: bool,
    assistant_message_action: Option<AssistantMessageAction>,
    user_message_rewind: UserMessageRewind,
    fintwind: gpui::WeakEntity<Fintwind>,
) -> AnyElement {
    let theme = *theme;
    let message_id = message.id;
    let copy_fintwind = fintwind.clone();
    let footer_color = if theme.is_dark {
        gpui::hsla(126.93 / 360.0, 0.000_000_1, 0.543_95, 1.0)
    } else {
        theme.text_ghost
    };
    let timestamp = div()
        .h(px(27.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .text_size(ui_px(11.5))
        .line_height(ui_px(14.0))
        .text_color(footer_color)
        .child(format_message_time(footer_time));
    let copy_button = div()
        .id(SharedString::from(format!("copy-message-{message_id}")))
        .w(px(30.0))
        .h(px(30.0))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_default()
        .hover(|element| element.bg(theme.overlay_strong))
        .child(icon(
            if copied {
                "icons/check.svg"
            } else {
                "icons/copy.svg"
            },
            15.0,
            footer_color,
        ))
        .tooltip(Tooltip::text(if copied {
            tr!("common.copied")
        } else {
            tr!("common.copy_message")
        }))
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_content.to_string()));
            let _ = copy_fintwind.update(cx, |this, cx| {
                this.show_message_copied(message_id, cx);
            });
        });
    let mut footer = div()
        .when(align_right, |element| element.w_full())
        .when(!align_right, |element| element.flex_none())
        .h(px(27.0))
        .flex()
        .items_center()
        .gap(px(1.0))
        .invisible()
        .group_hover(group_name, |element| element.visible())
        .when(!align_right, |element| element.ml(-px(7.0)))
        .when(align_right, |element| element.justify_end());

    if align_right {
        footer = footer.child(timestamp).child(copy_button);
    } else {
        footer = footer.child(copy_button);
        if let Some(action) = assistant_message_action {
            let fork_fintwind = fintwind.clone();
            let fork_icon = if action.preparing {
                motion::spin(icon("icons/loader-circle.svg", 15.0, footer_color))
            } else {
                icon("icons/fork.svg", 15.0, footer_color).into_any_element()
            };
            let fork_button = div()
                .id(SharedString::from(format!("fork-response-{message_id}")))
                .w(px(30.0))
                .h(px(30.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .when(!action.enabled && !action.preparing, |element| {
                    element.opacity(0.45)
                })
                .child(fork_icon)
                .tooltip(Tooltip::text(if action.enabled {
                    tr_cow!("session.fork_task")
                } else {
                    tr_cow!("session.forking_task")
                }));
            footer = footer.child(if action.enabled {
                fork_button
                    .hover(|element| element.bg(theme.overlay_strong))
                    .on_click(move |_, _, cx| {
                        let _ = fork_fintwind.update(cx, |this, cx| {
                            this.fork_session_from_response(
                                action.session_id,
                                action.turn_count,
                                cx,
                            );
                        });
                    })
            } else {
                fork_button
            });
        }
        footer = footer.child(timestamp);
    }

    if !matches!(user_message_rewind, UserMessageRewind::Hidden) {
        let edit_fintwind = fintwind;
        let ready_action = user_message_rewind.ready();
        footer = footer.child(
            div()
                .id(SharedString::from(format!(
                    "user-message-action-{message_id}"
                )))
                .w(px(27.0))
                .h(px(27.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .when(ready_action.is_some(), |element| {
                    element.hover(|element| element.bg(theme.overlay_strong))
                })
                .when(ready_action.is_none(), |element| element.opacity(0.45))
                .child(icon("icons/rewind.svg", 14.0, footer_color))
                .tooltip(Tooltip::text(rewind_tooltip_text(user_message_rewind)))
                .when_some(ready_action, |element, action| {
                    element.on_click(move |_, window, cx| {
                        let _ = edit_fintwind.update(cx, |this, cx| {
                            this.begin_message_edit(action, window, cx);
                        });
                    })
                }),
        );
    }

    footer.into_any_element()
}

/// The rewind affordance's tooltip: the action label when available, or why
/// it is blocked. Stays on borrowed locale data so the per-frame row builder
/// does not allocate.
fn rewind_tooltip_text(rewind: UserMessageRewind) -> std::borrow::Cow<'static, str> {
    match rewind {
        UserMessageRewind::Ready(_) | UserMessageRewind::Hidden => {
            tr_cow!("session.revert_to_here")
        }
        UserMessageRewind::Blocked(reason) => match reason {
            RewindUnavailableReason::NotTurnOpening => {
                tr_cow!("session.rewind_blocked_not_turn_opening")
            }
            RewindUnavailableReason::NotGitRepository => {
                tr_cow!("session.rewind_blocked_not_git")
            }
            RewindUnavailableReason::CheckpointError => {
                tr_cow!("session.rewind_blocked_checkpoint_error")
            }
            RewindUnavailableReason::SnapshotMissing => {
                tr_cow!("session.rewind_blocked_snapshot_missing")
            }
            RewindUnavailableReason::ProviderLinkMissing => {
                tr_cow!("session.rewind_blocked_provider_link")
            }
        },
    }
}

/// Everything one transcript message row needs to render itself. Bundled
/// because these travel together from `transcript_row` and nowhere else.
pub(super) struct MessageRender<'a> {
    pub(super) theme: &'a Theme,
    pub(super) message: &'a Message,
    pub(super) assistant_footer_copy_content: Option<SharedString>,
    pub(super) assistant_footer_time: Option<u64>,
    pub(super) assistant_before_footer: Option<AnyElement>,
    /// The settled turn's always-visible stats line — "Build · Model · 25.9s
    /// · 64.5 tok/s" — shown under the body (and the changed-files card)
    /// of the message that owns the response footer.
    pub(super) assistant_turn_stats: Option<SharedString>,
    pub(super) copied: bool,
    pub(super) assistant_message_action: Option<AssistantMessageAction>,
    pub(super) user_message_rewind: UserMessageRewind,
    pub(super) user_message_fill_width: bool,
    pub(super) message_edit_input: Option<Entity<ComposerInput>>,
    pub(super) attachment_menus: Vec<ContextMenuHandle>,
    pub(super) attachment_images: Vec<Option<Arc<gpui::Image>>>,
    /// Captured from the selected daemon before the virtualized row is built.
    /// A row is laid out while the root `Fintwind` entity is already updating, so
    /// it must not read that entity again just to decide whether Finder reveal
    /// is available.
    pub(super) attachments_can_reveal: bool,
    /// The parsed human or assistant body. System messages remain verbatim.
    pub(super) markdown: Option<&'a MarkdownView>,
    pub(super) ctx: &'a MarkdownCtx<'a>,
    /// Whether the settled compaction card discloses its summary. Only
    /// compaction rows read this; the subagent surfaces pass `false`.
    pub(super) compaction_expanded: bool,
    /// Stable focus identity for the compaction card's disclosure header.
    pub(super) compaction_focus: Option<FocusHandle>,
    pub(super) menu: ContextMenuHandle,
    pub(super) fintwind: gpui::WeakEntity<Fintwind>,
    pub(super) composer: Entity<ComposerInput>,
}

fn render_sent_message_attachments(
    message_id: Uuid,
    attachments: &[MessageAttachment],
    attachment_menus: &[ContextMenuHandle],
    attachment_images: &[Option<Arc<gpui::Image>>],
    can_reveal: bool,
    fintwind: &gpui::WeakEntity<Fintwind>,
    theme: &Theme,
) -> Option<AnyElement> {
    if attachments.is_empty() {
        return None;
    }
    let mut row = div()
        .max_w(px(540.0))
        .flex()
        .flex_wrap()
        .justify_end()
        .gap(px(8.0));
    for (index, attachment) in attachments.iter().enumerate() {
        let Some(menu) = attachment_menus.get(index) else {
            continue;
        };
        let icon_path = if attachment.is_dir {
            "icons/folder.svg"
        } else {
            right_panel::file_icon_for_path(&attachment.mention)
        };
        let attachment_image = attachment_images.get(index).and_then(|image| image.clone());
        let mut tile = div()
            .id(SharedString::from(format!(
                "message-{message_id}-attachment-{index}"
            )))
            .w(px(96.0))
            .h(px(80.0))
            .rounded(px(9.0))
            .overflow_hidden()
            .border_1()
            .border_color(theme.border)
            .bg(theme.inset)
            .track_focus(menu.trigger_focus_handle())
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .tooltip(Tooltip::text(attachment.name.clone()));
        if attachment.is_image {
            let key_menu = menu.clone();
            if let Some(attachment_image) = attachment_image.as_ref() {
                let preview_fintwind = fintwind.clone();
                let key_fintwind = fintwind.clone();
                let preview_image = attachment_image.clone();
                let key_image = attachment_image.clone();
                let preview_name = SharedString::from(attachment.name.clone());
                let key_name = preview_name.clone();
                tile = tile.child(
                    div()
                        .id(SharedString::from(format!(
                            "message-{message_id}-attachment-{index}-preview"
                        )))
                        .size_full()
                        .cursor_default()
                        .on_click(move |_, window, cx| {
                            let _ = preview_fintwind.update(cx, |this, cx| {
                                this.open_image_preview(
                                    preview_image.clone(),
                                    preview_name.clone(),
                                    window,
                                    cx,
                                );
                            });
                            cx.stop_propagation();
                        })
                        .child(
                            img(attachment_image.clone())
                                .size_full()
                                .object_fit(ObjectFit::Cover),
                        ),
                );
                tile = tile.on_key_down(move |event: &KeyDownEvent, window, cx| {
                    let key = event.keystroke.key.as_str();
                    if matches!(key, "enter" | "space") {
                        let _ = key_fintwind.update(cx, |this, cx| {
                            this.open_image_preview(
                                key_image.clone(),
                                key_name.clone(),
                                window,
                                cx,
                            );
                        });
                        cx.stop_propagation();
                    } else if key == "f10" && event.keystroke.modifiers.shift {
                        key_menu.open_context_menu(window, cx);
                        cx.stop_propagation();
                    }
                });
            } else {
                tile = tile
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("icons/file-types/image.svg", 18.0, theme.text_ghost)),
                    )
                    .on_key_down(move |event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "f10" && event.keystroke.modifiers.shift {
                            key_menu.open_context_menu(window, cx);
                            cx.stop_propagation();
                        }
                    });
            }
        } else {
            let key_menu = menu.clone();
            tile = tile.child(
                div()
                    .size_full()
                    .px(px(7.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(7.0))
                    .child(icon(icon_path, 18.0, theme.text_tertiary))
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_center()
                            .text_size(ui_px(9.5))
                            .text_color(theme.text_secondary)
                            .child(attachment.name.clone()),
                    ),
            );
            tile = tile.on_key_down(move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "f10" && event.keystroke.modifiers.shift {
                    key_menu.open_context_menu(window, cx);
                    cx.stop_propagation();
                }
            });
        }
        let reveal_path = attachment.path.clone();
        row = row.child(context_menu(
            tile,
            SharedString::from(format!("message-{message_id}-attachment-{index}-menu")),
            menu,
            move |_| image_preview::attachment_menu_items(reveal_path.clone(), can_reveal),
        ));
    }
    Some(row.into_any_element())
}

fn render_markdown_message_body<'a>(
    content: &str,
    markdown: Option<&'a MarkdownView>,
    theme: &Theme,
    ctx: &MarkdownCtx<'a>,
) -> AnyElement {
    markdown
        .and_then(|markdown| md::render::markdown(markdown, ctx))
        // Empty or not-yet-parsed content still needs a selectable fallback.
        .unwrap_or_else(|| {
            md::render::plain_text(
                content.to_owned(),
                md::render::sans_family(),
                FontWeight::NORMAL,
                theme.text,
                ctx,
            )
        })
}

pub(super) fn render_message(params: MessageRender, cx: &mut App) -> AnyElement {
    let MessageRender {
        theme,
        message,
        assistant_footer_copy_content,
        assistant_footer_time,
        assistant_before_footer,
        assistant_turn_stats,
        copied,
        assistant_message_action,
        user_message_rewind,
        user_message_fill_width,
        message_edit_input,
        attachment_menus,
        attachment_images,
        attachments_can_reveal,
        markdown,
        ctx,
        compaction_expanded,
        compaction_focus,
        menu,
        fintwind,
        composer,
    } = params;

    let ctx = ctx.clone().with_context_menu(menu.clone());
    let content = message.visible_content().to_owned();
    // "Copy Message" must match what the row presents. The terminal part of a
    // settled response stands in for the whole visible answer, so its menu
    // shares the footer's copy content — parts hidden behind "Worked for X"
    // stay out — rather than copying the final part alone.
    let menu_copy_content = assistant_footer_copy_content
        .clone()
        .unwrap_or_else(|| SharedString::from(content.clone()));
    let message_id = message.id;
    let role = message.role;
    let element = match role {
        MessageRole::User => {
            let group_name = SharedString::from(format!("user-message-{message_id}"));
            let mut column = div()
                .w_full()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(3.0))
                .group(group_name.clone());
            if let Some(attachments) = render_sent_message_attachments(
                message_id,
                &message.attachments,
                &attachment_menus,
                &attachment_images,
                attachments_can_reveal,
                &fintwind,
                theme,
            ) {
                column = column.child(attachments);
            }
            if let Some(edit_input) = message_edit_input {
                let can_submit = !edit_input.read(cx).content().trim().is_empty()
                    || !message.attachments.is_empty();
                let cancel_fintwind = fintwind.clone();
                let submit_fintwind = fintwind.clone();
                column = column.child(
                    div()
                        .w_full()
                        .max_w(px(540.0))
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .rounded(px(12.0))
                        .bg(theme.raised)
                        .pt(px(9.0))
                        .pb(px(8.0))
                        .child(div().w_full().flex_none().child(edit_input))
                        .child(
                            div()
                                .flex_none()
                                .mt(px(7.0))
                                .px(px(12.0))
                                .flex()
                                .justify_end()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "cancel-message-edit-{message_id}"
                                        )))
                                        .h(px(30.0))
                                        .px(px(11.0))
                                        .rounded(px(7.0))
                                        .border_1()
                                        .border_color(theme.border)
                                        .bg(theme.overlay)
                                        .flex()
                                        .items_center()
                                        .text_size(ui_px(12.5))
                                        .text_color(theme.text_secondary)
                                        .cursor_default()
                                        .hover(|element| element.bg(theme.overlay_strong))
                                        .child(tr_cow!("common.cancel"))
                                        .on_click(move |_, window, cx| {
                                            let _ = cancel_fintwind.update(cx, |this, cx| {
                                                this.cancel_message_edit(window, cx);
                                            });
                                        }),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "submit-message-edit-{message_id}"
                                        )))
                                        .h(px(30.0))
                                        .px(px(12.0))
                                        .rounded(px(7.0))
                                        .bg(if can_submit {
                                            theme.inverse
                                        } else {
                                            theme.overlay_strong
                                        })
                                        .flex()
                                        .items_center()
                                        .text_size(ui_px(12.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(if can_submit {
                                            theme.on_inverse
                                        } else {
                                            theme.text_ghost
                                        })
                                        .when(can_submit, |element| {
                                            element
                                                .cursor_default()
                                                .hover(|element| element.opacity(0.9))
                                        })
                                        .child(tr_cow!("common.send"))
                                        .on_click(move |_, _, cx| {
                                            if can_submit {
                                                let _ = submit_fintwind.update(cx, |this, cx| {
                                                    this.submit_message_edit(cx);
                                                });
                                            }
                                        }),
                                ),
                        ),
                );
            } else {
                if !content.trim().is_empty() {
                    let body = render_markdown_message_body(&content, markdown, theme, &ctx);
                    // `w_full()` + `max_w` on a column child measures height at
                    // the unclamped width (Taffy). Put width on the main axis
                    // so max_w is applied before the text is measured.
                    let bubble = div()
                        .max_w(px(540.0))
                        .min_w_0()
                        .when(user_message_fill_width, |element| element.w_full())
                        .rounded(px(12.0))
                        .bg(theme.raised)
                        .px(px(12.0))
                        .py(px(8.0))
                        .text_size(ui_px(14.0))
                        .line_height(ui_px(20.0))
                        .child(body);
                    column = column.child(if user_message_fill_width {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .child(bubble)
                    } else {
                        bubble
                    });
                }
                column = column.child(render_message_footer(
                    theme,
                    message,
                    message.created_at,
                    SharedString::from(content.clone()),
                    copied,
                    group_name,
                    true,
                    None,
                    user_message_rewind,
                    fintwind.clone(),
                ));
            }
            column
        }
        MessageRole::Assistant => {
            let group_name = SharedString::from(format!("assistant-message-{message_id}"));
            let body = render_markdown_message_body(&content, markdown, theme, &ctx);
            let mut column = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .py(px(4.0))
                .gap(px(3.0))
                .group(group_name.clone())
                .child(body);
            if let Some(before_footer) = assistant_before_footer {
                column = column.child(div().w_full().mt(px(12.0)).mb(px(3.0)).child(before_footer));
            }
            if assistant_turn_stats.is_some() || assistant_footer_copy_content.is_some() {
                column = column.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .when_some(assistant_turn_stats, |row, stats| {
                            row.child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .text_ellipsis()
                                    .text_size(ui_px(11.5))
                                    .line_height(ui_px(16.0))
                                    .text_color(theme.text_tertiary)
                                    .tooltip(Tooltip::text(stats.clone()))
                                    .child(stats),
                            )
                        })
                        .when_some(assistant_footer_copy_content, |row, copy_content| {
                            row.child(render_message_footer(
                                theme,
                                message,
                                assistant_footer_time.unwrap_or(message.created_at),
                                copy_content,
                                copied,
                                group_name,
                                false,
                                assistant_message_action,
                                UserMessageRewind::Hidden,
                                fintwind.clone(),
                            ))
                        }),
                );
            }
            column
        }
        MessageRole::System => div().w_full().flex().justify_center().child(
            div()
                .px(px(10.0))
                .py(px(4.0))
                .rounded_full()
                .bg(theme.overlay)
                .text_size(ui_px(11.0))
                .line_height(ui_px(16.0))
                .child(md::render::plain_text(
                    content.clone(),
                    md::render::sans_family(),
                    FontWeight::NORMAL,
                    theme.text_tertiary,
                    &ctx,
                )),
        ),
        MessageRole::Compaction => {
            // The settled summary is provider bookkeeping, not conversation:
            // it presents as one collapsed card styled after the tool
            // activity rows, and the full document discloses in place.
            let expanded = compaction_expanded;
            let surface = theme.surface.blend(theme.overlay.opacity(0.7));
            // The collapsed header previews the summary's first heading or
            // line — enough to tell which compaction this is without
            // unfurling a long document.
            let preview = content
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| {
                    line.trim_start_matches(['#', '*'])
                        .trim()
                        .trim_end_matches('*')
                        .trim()
                })
                .unwrap_or_default()
                .to_owned();
            let shows_preview = !expanded && !preview.is_empty();
            let header = div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .min_w_0()
                .w_full()
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_secondary)
                        .child(tr!("transcript.compaction")),
                )
                .when(shows_preview, |header| {
                    header.child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(preview.clone())),
                    )
                })
                .when(!shows_preview, |header| {
                    header.child(div().flex_1().min_w(px(0.0)))
                })
                .child(icon(
                    if expanded {
                        "icons/chevron-down.svg"
                    } else {
                        "icons/chevron-right.svg"
                    },
                    11.0,
                    theme.text_tertiary,
                ));
            let mut card = div()
                .w_full()
                .min_w_0()
                .rounded(px(9.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(surface)
                .flex()
                .flex_col();
            let click_fintwind = fintwind.clone();
            let key_fintwind = fintwind.clone();
            match compaction_focus {
                Some(focus) => {
                    card = card.child(
                        div()
                            .id(SharedString::from(format!("compaction-{message_id}")))
                            .track_focus(&focus)
                            .tab_index(0)
                            .px(px(10.0))
                            .h(px(30.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .cursor_default()
                            .text_size(ui_px(12.5))
                            .line_height(ui_px(17.0))
                            .focus_visible(|style| style.text_color(theme.text))
                            .hover(|style| style.text_color(theme.text))
                            .active(|style| style.text_color(theme.text_ghost))
                            .child(header)
                            .on_click(move |_, _, cx| {
                                let _ = click_fintwind.update(cx, |this, cx| {
                                    this.toggle_compaction_message(message_id, expanded, cx)
                                });
                            })
                            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    let _ = key_fintwind.update(cx, |this, cx| {
                                        this.toggle_compaction_message(message_id, expanded, cx)
                                    });
                                    cx.stop_propagation();
                                }
                            }),
                    );
                }
                // Compaction rows only render in the main transcript, which
                // always supplies a focus handle; this keeps a static header
                // if a surface without one ever shows the role.
                None => {
                    card = card.child(
                        div()
                            .px(px(10.0))
                            .h(px(30.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .child(header),
                    );
                }
            }
            if expanded {
                card = card.child(div().px(px(10.0)).pb(px(8.0)).min_w_0().child(
                    render_markdown_message_body(&content, markdown, theme, &ctx),
                ));
            }
            card
        }
    };

    let selection = ctx.selection().clone();
    context_menu(
        element.id(message_id),
        SharedString::from(format!("message-menu-{message_id}")),
        &menu,
        move |cx| {
            message_menu_items(
                &menu_copy_content,
                role,
                user_message_rewind,
                assistant_message_action,
                &selection,
                &composer,
                &fintwind,
                cx,
            )
        },
    )
}

/// The message row's context menu. Rebuilt on each open, so availability checks
/// here always reflect the current session state.
#[allow(clippy::too_many_arguments)]
fn message_menu_items(
    content: &str,
    role: MessageRole,
    user_message_rewind: UserMessageRewind,
    assistant_message_action: Option<AssistantMessageAction>,
    selection: &TranscriptSelection,
    composer: &Entity<ComposerInput>,
    fintwind: &gpui::WeakEntity<Fintwind>,
    _cx: &mut App,
) -> Vec<MenuItem> {
    let mut items = Vec::new();

    if let Some(selected) = selection.selection.borrow().selected_text() {
        items.push(MenuItem::new(tr!("common.copy_selection"), move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(selected.clone()));
        }));
    }

    let copy_content = content.to_owned();
    items.push(MenuItem::new(
        tr!("common.copy_message_title"),
        move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_content.clone()));
        },
    ));

    if role == MessageRole::User && user_message_rewind.ready().is_none() {
        let composer = composer.clone();
        let edit_content = content.to_owned();
        items.push(MenuItem::new(
            tr!("common.copy_to_composer"),
            move |window, cx| {
                composer.update(cx, |composer, cx| {
                    composer.set_content(edit_content.clone(), cx);
                });
                let focus_handle = composer.read(cx).focus();
                window.focus(&focus_handle, cx);
            },
        ));
    }

    if let Some(code) = fenced_code(content) {
        items.push(MenuItem::new(tr!("common.copy_code"), move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
        }));
    }

    match user_message_rewind {
        UserMessageRewind::Hidden => {}
        UserMessageRewind::Ready(action) => {
            let fintwind = fintwind.clone();
            items.push(MenuItem::Separator);
            items.push(
                MenuItem::new(tr!("session.revert_to_here_title"), move |window, cx| {
                    let _ = fintwind.update(cx, |this, cx| {
                        this.begin_message_edit(action, window, cx);
                    });
                })
                .icon("icons/rewind.svg"),
            );
        }
        UserMessageRewind::Blocked(_) => {
            // The menu has no tooltips, so the disabled entry carries the
            // reason itself as its label.
            items.push(MenuItem::Separator);
            items.push(
                MenuItem::new(
                    rewind_tooltip_text(user_message_rewind).into_owned(),
                    |_, _| {},
                )
                .icon("icons/rewind.svg")
                .disabled(true),
            );
        }
    }

    if let Some(action) = assistant_message_action {
        let fintwind = fintwind.clone();
        items.push(MenuItem::Separator);
        items.push(
            MenuItem::new(
                if action.enabled {
                    tr!("session.fork_task_title")
                } else {
                    tr!("session.forking_task_title")
                },
                move |_, cx| {
                    let _ = fintwind.update(cx, |this, cx| {
                        this.fork_session_from_response(action.session_id, action.turn_count, cx);
                    });
                },
            )
            .icon("icons/fork.svg")
            .disabled(!action.enabled),
        );
    }

    items
}

pub(super) fn fenced_code(content: &str) -> Option<String> {
    let mut code_blocks = Vec::new();
    let mut segments = content.split("```");
    let _ = segments.next();
    while let Some(fenced) = segments.next() {
        let (language, code) = fenced
            .split_once('\n')
            .map(|(language, code)| (language.trim(), code))
            .unwrap_or(("", fenced));
        let code = if language.is_empty() && !fenced.contains('\n') {
            fenced
        } else {
            code
        };
        if !code.trim().is_empty() {
            code_blocks.push(code.trim_end().to_owned());
        }
        let _ = segments.next();
    }
    (!code_blocks.is_empty()).then(|| code_blocks.join("\n\n"))
}

pub(super) fn activity_summary(activities: &[ActivityItem]) -> String {
    let mut counts: Vec<(crate::model::ActivityKind, usize)> = Vec::new();
    for activity in activities {
        if let Some(entry) = counts.iter_mut().find(|(kind, _)| *kind == activity.kind) {
            entry.1 += 1;
        } else {
            counts.push((activity.kind, 1));
        }
    }
    let parts = counts
        .into_iter()
        .map(|(kind, count)| {
            let (singular, plural) = activity_noun(kind);
            tr!(
                "activity.count",
                count = count,
                activity = if count == 1 { singular } else { plural }
            )
        })
        .collect::<Vec<_>>();
    let running = activities.iter().any(|activity| !activity.complete);
    if running {
        tr!("activity.running", activities = parts.join(" · "))
    } else {
        tr!("activity.ran", activities = parts.join(" · "))
    }
}

pub(super) fn activity_header_title(
    activities: &[ActivityItem],
    live_turn: bool,
    live_reasoning_id: Option<Uuid>,
) -> String {
    if live_turn && let Some(activity) = activities.last() {
        return activity.reasoning.as_ref().map_or_else(
            || activity_display_title(activity),
            |reasoning| reasoning_activity_title(reasoning, live_reasoning_id == Some(activity.id)),
        );
    }

    activity_summary(activities)
}

fn tool_name_leaf(name: &str) -> &str {
    let name = name.trim();
    let leaf = name.rsplit("__").next().unwrap_or(name);
    leaf.rsplit([':', '.', '/']).next().unwrap_or(leaf)
}

fn is_ask_user_question(activity: &ActivityItem) -> bool {
    activity.kind == crate::model::ActivityKind::Tool
        && matches!(
            tool_name_leaf(&activity.title)
                .chars()
                .filter(|character| !matches!(*character, '_' | '-' | ' '))
                .flat_map(char::to_lowercase)
                .collect::<String>()
                .as_str(),
            "askuserquestion" | "question"
        )
}

pub(super) fn activity_display_title(activity: &ActivityItem) -> String {
    if activity.kind == crate::model::ActivityKind::Reasoning {
        return activity.reasoning.as_ref().map_or_else(
            || activity_action_label(activity),
            |reasoning| reasoning_activity_title(reasoning, false),
        );
    }
    let action = activity_action_label(activity);
    let detail = activity_row_detail(activity, false);
    if detail.is_empty() || detail == action {
        action
    } else {
        format!("{action} {detail}")
    }
}

pub(super) fn activity_action_label(activity: &ActivityItem) -> String {
    if activity.kind == crate::model::ActivityKind::Reasoning {
        return "thinking".to_owned();
    }
    let title = activity.title.trim();
    if title.is_empty() {
        tr!("activity.tool")
    } else {
        title.to_owned()
    }
}

pub(super) fn activity_row_detail(activity: &ActivityItem, reasoning_live: bool) -> String {
    use crate::model::ActivityKind;

    match activity.kind {
        ActivityKind::Reasoning => activity.reasoning.as_ref().map_or_else(
            || activity.title.clone(),
            |reasoning| reasoning_activity_title(reasoning, reasoning_live),
        ),
        ActivityKind::Command => activity
            .display_target
            .clone()
            .or_else(|| activity.display_description.clone())
            .unwrap_or_default(),
        ActivityKind::FileChange => match activity.file_changes.as_slice() {
            [change] => change.display_name().to_owned(),
            changes if !changes.is_empty() => {
                tr!("activity.file_count", count = changes.len())
            }
            _ => String::new(),
        },
        ActivityKind::FileRead | ActivityKind::FileList => activity
            .display_target
            .as_deref()
            .map(activity_path_name)
            .unwrap_or_default(),
        ActivityKind::FileSearch | ActivityKind::Search | ActivityKind::Plan => {
            activity.display_target.clone().unwrap_or_default()
        }
        ActivityKind::Tool if is_ask_user_question(activity) => String::new(),
        ActivityKind::Tool => activity.display_target.clone().unwrap_or_default(),
    }
}

pub(super) fn reasoning_activity_title(reasoning: &ReasoningBlock, live: bool) -> String {
    if live {
        tr!("transcript.thinking")
    } else {
        tr!(
            "transcript.thought_for",
            duration = format_worked_duration(
                reasoning
                    .finished_at_ms
                    .saturating_sub(reasoning.started_at_ms)
                    .div_ceil(1000)
                    .max(1)
            )
        )
    }
}

fn activity_path_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_owned()
}

/// Whether this activity's expanded view shows a diff instead of the tool
/// arguments that produced it.
pub(super) fn activity_shows_diff(activity: &ActivityItem) -> bool {
    activity.kind == ActivityKind::FileChange
        && activity
            .file_changes
            .iter()
            .any(|change| change.diff.is_some())
}

pub(super) fn activity_file_change_stats(activity: &ActivityItem) -> Option<(u64, u64)> {
    if activity.kind != crate::model::ActivityKind::FileChange
        || !activity.complete
        || activity.failed
        || activity.file_changes.is_empty()
    {
        return None;
    }
    let additions = activity
        .file_changes
        .iter()
        .map(|change| change.additions)
        .sum::<Option<u64>>()?;
    let deletions = activity
        .file_changes
        .iter()
        .map(|change| change.deletions)
        .sum::<Option<u64>>()?;
    Some((additions, deletions))
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum ActivityDisclosureSectionKind {
    Command,
    Arguments,
    Output,
    Detail,
}

impl ActivityDisclosureSectionKind {
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Arguments => "arguments",
            Self::Output => "output",
            Self::Detail => "detail",
        }
    }

    pub(super) fn label(self) -> Option<String> {
        match self {
            Self::Command => Some(tr!("activity.command_detail")),
            Self::Arguments => Some(tr!("activity.arguments")),
            Self::Output => Some(tr!("activity.output")),
            Self::Detail => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityDisclosureSection {
    pub(super) kind: ActivityDisclosureSectionKind,
    pub(super) content: String,
}

pub(super) fn activity_disclosure_sections(
    activity: &ActivityItem,
) -> Vec<ActivityDisclosureSection> {
    let mut sections = Vec::new();
    if activity.kind == ActivityKind::Command {
        if let Some(command) = activity
            .arguments
            .as_deref()
            .or(activity.display_target.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            sections.push(ActivityDisclosureSection {
                kind: ActivityDisclosureSectionKind::Command,
                content: command.to_owned(),
            });
        }
        if let Some(output) = activity
            .output
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            sections.push(ActivityDisclosureSection {
                kind: ActivityDisclosureSectionKind::Output,
                content: output.to_owned(),
            });
        } else if !activity.image_urls.is_empty() {
            sections.push(ActivityDisclosureSection {
                kind: ActivityDisclosureSectionKind::Output,
                content: String::new(),
            });
        }
        return sections;
    }
    // An edit renders as a diff, which says everything the raw arguments would
    // and reads. What the tool replied is only worth the room when it failed.
    let shows_diff = activity_shows_diff(activity);
    if let Some(arguments) = activity
        .arguments
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|_| !shows_diff)
        .map(unwrap_code_mode_arguments)
    {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Arguments,
            content: arguments,
        });
    }
    if let Some(output) = activity
        .output
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|_| !shows_diff || activity.failed)
    {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Output,
            content: output.to_owned(),
        });
    } else if !activity.image_urls.is_empty() {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Output,
            content: String::new(),
        });
    }
    if sections.is_empty()
        && let Some(detail) = activity
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Detail,
            content: detail.to_owned(),
        });
    }
    sections
}

fn unwrap_code_mode_arguments(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return arguments.to_owned();
    };
    let Some(object) = value.as_object() else {
        return arguments.to_owned();
    };
    if object.len() == 1
        && let Some(code) = object.get("code").and_then(serde_json::Value::as_str)
        && !code.trim().is_empty()
    {
        return code.to_owned();
    }
    arguments.to_owned()
}

pub(super) fn activity_preview(activity: &ActivityItem) -> String {
    let detail = activity.detail.as_deref().unwrap_or_default().trim();
    if detail.eq_ignore_ascii_case("failed")
        && let Some(output) = activity.output.as_deref()
        && let Some(first_line) = output.lines().find(|line| !line.trim().is_empty())
    {
        return first_line.trim().to_owned();
    }
    if (detail.is_empty() || detail.eq_ignore_ascii_case("failed"))
        && !activity.image_urls.is_empty()
    {
        return tr!("activity.image_output");
    }
    detail.to_owned()
}

#[cfg(test)]
mod message_time_tests {
    use super::*;
    use chrono::TimeZone;

    /// Test-only rendering of disclosure sections into plain text; production
    /// renders them interactively via [`activity_disclosure_sections`].
    fn activity_disclosure_text(activity: &ActivityItem) -> Option<String> {
        let sections = activity_disclosure_sections(activity);
        (!sections.is_empty()).then(|| {
            sections
                .into_iter()
                .map(
                    |section| match (section.kind.label(), section.content.is_empty()) {
                        (Some(label), false) => format!("{label}\n{}", section.content),
                        (Some(label), true) => label.to_owned(),
                        (None, _) => section.content,
                    },
                )
                .collect::<Vec<_>>()
                .join("\n\n")
        })
    }

    fn local_datetime(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("test date should be valid in the local timezone")
    }

    fn unix_seconds(timestamp: DateTime<Local>) -> u64 {
        timestamp
            .timestamp()
            .try_into()
            .expect("test date should have a positive Unix timestamp")
    }

    #[test]
    fn message_time_includes_calendar_context_for_older_messages() {
        let now = local_datetime(2026, 8, 9, 16, 0); // Sunday

        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 8, 9, 9, 5)), now),
            "9:05 AM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 8, 8, 17, 0)), now),
            "Yesterday 5:00 PM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 8, 7, 13, 12)), now),
            "Friday 1:12 PM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 5, 12, 23, 0)), now),
            "May 12th, 11:00 PM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2024, 8, 4, 11, 0)), now),
            "Aug 4th 2024, 11:00 AM"
        );
    }

    #[test]
    fn message_time_uses_correct_ordinal_suffixes() {
        let now = local_datetime(2026, 8, 9, 16, 0);

        for (day, suffix) in [
            (1, "st"),
            (2, "nd"),
            (3, "rd"),
            (11, "th"),
            (12, "th"),
            (13, "th"),
            (21, "st"),
        ] {
            let formatted =
                format_message_time_at(unix_seconds(local_datetime(2026, 5, day, 9, 0)), now);
            assert!(formatted.starts_with(&format!("May {day}{suffix},")));
        }
    }

    #[test]
    fn activity_disclosure_keeps_arguments_and_output() {
        let activity = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "Use Helium",
            Some("failed".into()),
            true,
        )
        .with_arguments(Some("{\n  \"actions\": []\n}".into()))
        .with_output(Some("tool closed its session".into()))
        .with_failed(true);

        assert_eq!(
            activity_disclosure_sections(&activity),
            vec![
                ActivityDisclosureSection {
                    kind: ActivityDisclosureSectionKind::Arguments,
                    content: "{\n  \"actions\": []\n}".into(),
                },
                ActivityDisclosureSection {
                    kind: ActivityDisclosureSectionKind::Output,
                    content: "tool closed its session".into(),
                },
            ]
        );
        assert_eq!(
            activity_disclosure_text(&activity).as_deref(),
            Some("Arguments\n{\n  \"actions\": []\n}\n\nOutput\ntool closed its session")
        );
        assert_eq!(activity_preview(&activity), "tool closed its session");

        let image_only = ActivityItem::new(
            Some("tool-2".into()),
            crate::model::ActivityKind::Tool,
            "Screenshot",
            None,
            true,
        )
        .with_image_urls(vec!["data:image/png;base64,aGVsbG8=".into()]);
        assert_eq!(
            activity_disclosure_text(&image_only).as_deref(),
            Some("Output")
        );
        assert_eq!(activity_preview(&image_only), "Image output");
    }

    #[test]
    fn command_disclosure_shows_only_the_command_and_output() {
        let activity = ActivityItem::new(
            Some("command-1".into()),
            crate::model::ActivityKind::Command,
            "bash",
            Some("Completed".into()),
            true,
        )
        .with_arguments(Some(
            r#"{"command":"git status --short","description":"Check status"}"#.into(),
        ))
        .with_output(Some("clean".into()));

        assert_eq!(
            activity_disclosure_sections(&activity),
            vec![
                ActivityDisclosureSection {
                    kind: ActivityDisclosureSectionKind::Command,
                    content: "git status --short".into(),
                },
                ActivityDisclosureSection {
                    kind: ActivityDisclosureSectionKind::Output,
                    content: "clean".into(),
                },
            ]
        );
        assert_eq!(
            activity_disclosure_text(&activity).as_deref(),
            Some("Command\ngit status --short\n\nOutput\nclean")
        );
    }

    #[test]
    fn activity_display_title_keeps_the_provider_tool_name() {
        let titled = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "Js",
            None,
            true,
        )
        .with_arguments(Some(
            r#"{"title":"Inspect Helium browser","code":"sky.get_app_state()"}"#.into(),
        ));
        let untitled = ActivityItem::new(
            Some("tool-2".into()),
            crate::model::ActivityKind::Tool,
            "Js",
            None,
            true,
        )
        .with_arguments(Some(r#"{"code":"sky.list_apps()"}"#.into()));

        assert_eq!(activity_action_label(&titled), "Js");
        assert_eq!(activity_display_title(&titled), "Js Inspect Helium browser");
        assert_eq!(activity_action_label(&untitled), "Js");
        assert_eq!(activity_display_title(&untitled), "Js sky.list_apps()");
    }

    #[test]
    fn generic_tool_rows_keep_the_raw_provider_name() {
        let named = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "mcp__threads__create_thread",
            None,
            true,
        );
        let unnamed = ActivityItem::new(
            Some("tool-2".into()),
            crate::model::ActivityKind::Tool,
            "Tool",
            None,
            true,
        );

        assert_eq!(activity_action_label(&named), "mcp__threads__create_thread");
        assert_eq!(activity_row_detail(&named, false), "");
        assert_eq!(
            activity_display_title(&named),
            "mcp__threads__create_thread"
        );
        assert_eq!(activity_action_label(&unnamed), "Tool");
        assert_eq!(activity_row_detail(&unnamed, false), "");
    }

    #[test]
    fn ask_user_question_keeps_the_provider_tool_name() {
        let activity = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "AskUserQuestion",
            None,
            true,
        )
        .with_arguments(Some(r#"{"questions":[]}"#.into()));

        assert_eq!(activity_action_label(&activity), "AskUserQuestion");
        assert_eq!(activity_row_detail(&activity, false), "");
        assert_eq!(activity_display_title(&activity), "AskUserQuestion");
    }

    #[test]
    fn opencode_question_tool_keeps_the_raw_name() {
        let activity = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "question",
            None,
            false,
        )
        .with_arguments(Some(
            r#"{"questions":[{"question":"Favorite color?","header":"Color","options":[{"label":"Red"}]}]}"#.to_string(),
        ));

        assert_eq!(activity_action_label(&activity), "question");
        assert_eq!(activity_row_detail(&activity, false), "");
        assert_eq!(activity_display_title(&activity), "question");
    }

    #[test]
    fn live_activity_header_tracks_the_latest_child_until_the_turn_settles() {
        let reasoning = ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "Inspecting history".into(),
                started_at_ms: 1_000,
                finished_at_ms: 2_000,
            },
            true,
        );
        let command = ActivityItem::new(
            Some("command-1".into()),
            crate::model::ActivityKind::Command,
            "bash",
            None,
            false,
        )
        .with_arguments(Some(
            serde_json::json!({"command": "git log --oneline -15"}).to_string(),
        ));
        let mut activities = vec![reasoning, command];

        assert_eq!(
            activity_header_title(&activities, true, None),
            "bash git log --oneline -15"
        );
        activities[1].complete = true;
        assert_eq!(
            activity_header_title(&activities, true, None),
            "bash git log --oneline -15"
        );
        assert_eq!(
            activity_header_title(&activities, false, None),
            "Ran 1 thought · 1 command"
        );
        assert_eq!(activity_action_label(&activities[1]), "bash");
        assert_eq!(
            activity_row_detail(&activities[1], false),
            "git log --oneline -15"
        );
    }

    #[test]
    fn file_edit_title_and_stats_follow_the_activity_state() {
        let mut activity = ActivityItem::new(
            Some("edit-1".into()),
            crate::model::ActivityKind::FileChange,
            "apply_patch",
            None,
            false,
        )
        .with_arguments(Some(
            serde_json::json!({
                "patch": "*** Begin Patch\n*** Update File: /tmp/fintwind/src/app.rs\n@@\n-old\n+new\n+more\n*** End Patch"
            })
            .to_string(),
        ));

        assert_eq!(activity_action_label(&activity), "apply_patch");
        assert_eq!(activity_display_title(&activity), "apply_patch app.rs");
        assert_eq!(activity_file_change_stats(&activity), None);

        activity.complete = true;
        assert_eq!(activity_display_title(&activity), "apply_patch app.rs");
        assert_eq!(activity_file_change_stats(&activity), Some((2, 1)));

        activity.failed = true;
        assert_eq!(activity_display_title(&activity), "apply_patch app.rs");
        assert_eq!(activity_file_change_stats(&activity), None);
    }

    #[test]
    fn multi_file_edits_use_a_compact_count() {
        let activity = ActivityItem::new(
            Some("edit-2".into()),
            crate::model::ActivityKind::FileChange,
            "apply_patch",
            None,
            true,
        )
        .with_arguments(Some(
            serde_json::json!({
                "patch": "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-a\n+b\n*** Update File: src/b.rs\n@@\n-c\n+d\n*** End Patch"
            })
            .to_string(),
        ));

        assert_eq!(activity_display_title(&activity), "apply_patch 2 files");
        assert_eq!(activity_file_change_stats(&activity), Some((2, 2)));
    }

    #[test]
    fn file_tool_titles_include_the_target_and_state() {
        let mut read = ActivityItem::new(
            Some("read-1".into()),
            crate::model::ActivityKind::FileRead,
            "read",
            None,
            false,
        )
        .with_arguments(Some(
            serde_json::json!({"filePath": "/tmp/fintwind/src/model.rs"}).to_string(),
        ));
        assert_eq!(activity_action_label(&read), "read");
        assert_eq!(activity_display_title(&read), "read model.rs");
        read.complete = true;
        assert_eq!(activity_display_title(&read), "read model.rs");
        read.failed = true;
        assert_eq!(activity_display_title(&read), "read model.rs");

        let search = ActivityItem::new(
            Some("grep-1".into()),
            crate::model::ActivityKind::FileSearch,
            "grep",
            None,
            true,
        )
        .with_arguments(Some(
            serde_json::json!({"pattern": "ActivityKind"}).to_string(),
        ));
        assert_eq!(activity_action_label(&search), "grep");
        assert_eq!(activity_display_title(&search), "grep ActivityKind");

        let list = ActivityItem::new(
            Some("list-1".into()),
            crate::model::ActivityKind::FileList,
            "ls",
            None,
            false,
        )
        .with_arguments(Some(
            serde_json::json!({"path": "/tmp/fintwind/src"}).to_string(),
        ));
        assert_eq!(activity_action_label(&list), "ls");
        assert_eq!(activity_display_title(&list), "ls src");

        let custom = ActivityItem::new(
            Some("read-2".into()),
            crate::model::ActivityKind::FileRead,
            "Inspect generated manifest",
            None,
            true,
        );
        assert_eq!(
            activity_display_title(&custom),
            "Inspect generated manifest"
        );
        assert_eq!(activity_action_label(&custom), "Inspect generated manifest");
    }

    #[test]
    fn command_web_search_and_plan_titles_include_their_state() {
        let mut command = ActivityItem::new(
            Some("command-1".into()),
            crate::model::ActivityKind::Command,
            "bash",
            None,
            true,
        )
        .with_arguments(Some(
            serde_json::json!({
                "description": "Run focused tests",
                "command": "cargo test activity"
            })
            .to_string(),
        ));
        assert_eq!(activity_action_label(&command), "bash");
        assert_eq!(activity_display_title(&command), "bash cargo test activity");
        command.complete = false;
        assert_eq!(activity_display_title(&command), "bash cargo test activity");

        let web_search = ActivityItem::new(
            Some("search-1".into()),
            crate::model::ActivityKind::Search,
            "web_search",
            None,
            true,
        )
        .with_arguments(Some(
            serde_json::json!({"query": "Fintwind GPUI"}).to_string(),
        ));
        assert_eq!(activity_action_label(&web_search), "web_search");
        assert_eq!(
            activity_display_title(&web_search),
            "web_search Fintwind GPUI"
        );

        let plan = ActivityItem::new(
            Some("plan-1".into()),
            crate::model::ActivityKind::Plan,
            "update_plan",
            None,
            false,
        );
        assert_eq!(activity_action_label(&plan), "update_plan");
        assert_eq!(activity_display_title(&plan), "update_plan");
    }

    #[test]
    fn execute_rows_use_codemode_code_and_nested_tool_calls() {
        let mut execute = ActivityItem::new(
            Some("call_execute".into()),
            crate::model::ActivityKind::Tool,
            "execute",
            None,
            false,
        )
        .with_arguments(Some(
            serde_json::json!({
                "code": "return await tools.context7.query_docs({ libraryId: '/opencode' })"
            })
            .to_string(),
        ));
        assert_eq!(activity_action_label(&execute), "execute");
        assert_eq!(
            activity_display_title(&execute),
            "execute return await tools.context7.query_docs({ libraryId: '/opencode' })"
        );
        assert_eq!(
            activity_disclosure_text(&execute).as_deref(),
            Some("Arguments\nreturn await tools.context7.query_docs({ libraryId: '/opencode' })")
        );

        execute = execute.with_tool_metadata(Some(&serde_json::json!({
            "toolCalls": [
                {"tool": "context7.query_docs", "status": "running"},
                {"tool": "context7.resolve_library_id", "status": "completed"}
            ]
        })));
        assert_eq!(
            activity_display_title(&execute),
            "execute context7.query_docs · context7.resolve_library_id"
        );
    }
}
