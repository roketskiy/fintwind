use crate::theme::ui_px;

use super::*;

use anyhow::Context as _;
use base64::Engine as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ComposerSubmitAction {
    Send,
    Preparing,
    Stop,
}

pub(super) fn composer_submit_action(
    status: Option<SessionStatus>,
    preparing: bool,
) -> ComposerSubmitAction {
    if preparing {
        ComposerSubmitAction::Preparing
    } else if status.is_some_and(SessionStatus::is_busy) {
        ComposerSubmitAction::Stop
    } else {
        ComposerSubmitAction::Send
    }
}

/// Chat-column group for an external file drag. The column accepts the drop;
/// the composer card watches the group so the drop ring shows while the
/// pointer is still over the transcript, not only once it reaches the card.
pub(super) const CHAT_FILE_DROP_GROUP: &str = "chat-file-drop";

#[derive(Clone)]
struct QueuedMessageDrag {
    session_id: Uuid,
    message_id: Uuid,
    content: SharedString,
}

struct QueuedMessageDragPreview {
    content: SharedString,
    cursor_offset: gpui::Point<Pixels>,
}

impl Render for QueuedMessageDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        div()
            .pl(self.cursor_offset.x)
            .pt(self.cursor_offset.y)
            .child(
                div()
                    .w(px(320.0))
                    .h(px(32.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(theme.accent)
                    .bg(theme.composer)
                    .shadow_md()
                    .child(icon("icons/queue.svg", 13.0, theme.text_tertiary))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(13.0))
                            .text_color(theme.text)
                            .child(self.content.clone()),
                    ),
            )
    }
}

// ── Control-row width budget ─────────────────────────────────────────────
//
// The row is model · traits · access · mode · … · send. Every label in it
// gives way before any control does: a demoted chip keeps its icon and its
// hit area and moves its label into a tooltip, so a narrow column costs
// legibility and never reachability. The send button is the one control
// that is never demoted, because a composer whose submit button is missing
// is a composer that cannot be used.
//
// The order is what the demotion walks. The traits chip refines a model the
// row already names, so it goes first. The mode's icon already differs
// between build and plan. The permission level is what tells the reader
// what the agent may do unattended, so it outlasts the mode. The model name
// is last: it is the longest label, but it is also the one that says which
// model is answering, and the logo beside it only names the company.

/// Gap between two adjacent row children.
const ROW_GAP: f32 = 4.0;
/// The card's own border and the row's horizontal padding. Neither is
/// available to a chip.
const CARD_BORDER: f32 = 1.0;
const ROW_INSET_X: f32 = 10.0;
/// Padding between the chat column and the card, which the row's width is
/// measured inside of.
const CARD_GUTTER_X: f32 = 20.0;
/// The chat column's left border, drawn whenever the sidebar is visible.
/// It comes out of the same width the chips draw in.
const CHAT_COLUMN_BORDER: f32 = 1.0;
/// The send button, plus the queue button that stands in beside it while a
/// turn is running with a draft waiting.
const SEND_BUTTON: f32 = 30.0;
const SEND_PAIR_GAP: f32 = 6.0;
/// The widest the model name ever asks for. Past it the name ellipsizes:
/// a longer name would only push the other chips out, and the logo plus a
/// tooltip still name the model.
const MODEL_CHIP_MAX_LABEL: f32 = 210.0;

/// What the row's chips measure at the labels the session currently shows.
///
/// Every field is in pixels on the row's own content box, so the caller
/// resolves the row's insets, the card's border and the width cap once and
/// the arithmetic below stays about the chips alone.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ComposerRowWidths {
    /// Label widths of the demotable chips, absent where the chip itself
    /// is absent. The model chip is always present, so its logo is always
    /// charged for.
    pub model: Option<f32>,
    pub traits: Option<f32>,
    pub interaction: Option<f32>,
    pub access: Option<f32>,
    /// Width the row spends before any chip: its insets, the card's
    /// border, and the send button or the pair of them.
    pub fixed: f32,
}

impl ComposerRowWidths {
    /// What the row wants with every label showing.
    fn total(&self) -> f32 {
        let chips = self
            .model
            .into_iter()
            .chain(self.traits)
            .chain(self.interaction)
            .chain(self.access)
            .map(|label| chip_width(Some(label)))
            .sum::<f32>();
        // The row is model · traits · access · mode · spacer · send, so it
        // has five gaps between six children — four when the traits chip is
        // absent. Counting one gap per demotable chip comes to four, which
        // under-counts and lets the row overflow the card, so the full count
        // is charged here. The one-gap margin when the traits chip is absent
        // is deliberate: it makes the budget slightly conservative, demoting
        // a label a hair early rather than risking an overflow.
        self.fixed + chips + 5.0 * ROW_GAP
    }
}

/// The traits a model offers, resolved against what the session has picked.
///
/// Split out of the chip's builder because the row's width budget needs the
/// label before the chip exists, and two resolutions of the same choice
/// could disagree about what the chip says. It borrows the model so the
/// menu can read its option lists without a second lookup.
struct ModelTraits<'a> {
    model: &'a ProviderModel,
    effort: Option<String>,
    tier: String,
    window: Option<String>,
    /// The chip's label: the effort, or the tier when the model offers no
    /// efforts, plus a non-default context window — which changes what the
    /// session costs and how much it can hold, so it reads on the chip
    /// rather than only inside the menu.
    label: String,
    /// The `fast` tier, which the chip marks with a zap.
    fast: bool,
}

impl<'a> ModelTraits<'a> {
    /// `None` when the model offers nothing to choose here, which is also
    /// when the chip does not render.
    fn resolve(session: &AgentSession, model: &'a ProviderModel) -> Option<Self> {
        if model.reasoning_efforts.is_empty()
            && model.service_tiers.is_empty()
            && model.context_windows.is_empty()
        {
            return None;
        }
        let effort = session
            .reasoning_effort
            .as_deref()
            .filter(|selected| {
                model
                    .reasoning_efforts
                    .iter()
                    .any(|option| option.id == *selected)
            })
            .or(model.default_reasoning_effort.as_deref())
            .or_else(|| {
                model
                    .reasoning_efforts
                    .first()
                    .map(|option| option.id.as_str())
            })
            .map(str::to_owned);
        let effort_label = effort.as_deref().and_then(|selected| {
            model
                .reasoning_efforts
                .iter()
                .find(|option| option.id == selected)
                .map(|option| option.label.clone())
        });

        let tier = session
            .service_tier
            .as_deref()
            .filter(|selected| {
                *selected == "default"
                    || model
                        .service_tiers
                        .iter()
                        .any(|option| option.id == *selected)
            })
            .or(model.default_service_tier.as_deref())
            .unwrap_or("default")
            .to_owned();
        let tier_label = if tier == "default" {
            tr!("models.standard")
        } else {
            model
                .service_tiers
                .iter()
                .find(|option| option.id == tier)
                .map(|option| option.label.clone())
                .unwrap_or_else(|| tier.clone())
        };

        let window = session
            .context_window
            .as_deref()
            .filter(|selected| {
                model
                    .context_windows
                    .iter()
                    .any(|option| option.id == *selected)
            })
            .or(model.default_context_window.as_deref())
            .or_else(|| {
                model
                    .context_windows
                    .first()
                    .map(|option| option.id.as_str())
            })
            .map(str::to_owned);
        let window_label = window
            .as_deref()
            .filter(|selected| model.default_context_window.as_deref() != Some(selected))
            .and_then(|selected| {
                model
                    .context_windows
                    .iter()
                    .find(|option| option.id == selected)
                    .map(|option| option.label.clone())
            });

        let label = match (
            effort_label.unwrap_or_else(|| tier_label.clone()),
            window_label,
        ) {
            (label, Some(window)) => format!("{label} · {window}"),
            (label, None) => label,
        };
        let fast = tier == "fast" || tier_label.eq_ignore_ascii_case("fast");
        Some(Self {
            model,
            effort,
            tier,
            window,
            label,
            fast,
        })
    }
}

/// A chip label and the width it measures at, kept so the row's budget is
/// not re-shaped every frame.
///
/// The row is rebuilt on every frame the window draws, but its labels only
/// change when the session's model, tier or permission mode does. The
/// measurement is therefore taken once per label and reused, and dropped
/// wholesale when the UI font or text scale changes — a stale width from a
/// different font is worse than no cache at all.
struct ChipLabelWidth {
    text: SharedString,
    width: f32,
}

#[derive(Default)]
pub(super) struct ComposerRowCache {
    /// Font and scale the measurements below were taken at.
    metrics: Option<(u64, f32)>,
    model: Option<ChipLabelWidth>,
    traits: Option<ChipLabelWidth>,
    interaction: Option<ChipLabelWidth>,
    access: Option<ChipLabelWidth>,
}

impl ComposerRowCache {
    /// Drop every measurement when the text metrics move.
    fn sync_metrics(&mut self) {
        let metrics = (
            crate::theme::font_generation(),
            crate::theme::ui_text_scale(),
        );
        if self.metrics != Some(metrics) {
            self.metrics = Some(metrics);
            self.model = None;
            self.traits = None;
            self.interaction = None;
            self.access = None;
        }
    }

    fn width(slot: &mut Option<ChipLabelWidth>, text: &str, window: &Window) -> f32 {
        if let Some(measured) = slot.as_ref() {
            if measured.text.as_ref() == text {
                return measured.width;
            }
        }
        let width = measure_chip_label(text, window);
        *slot = Some(ChipLabelWidth {
            text: text.into(),
            width,
        });
        width
    }
}

/// Shape one chip label at the size the chips draw, in the UI font they
/// inherit. Measured rather than guessed so a long model name or a
/// translated permission label costs the row exactly what it will paint.
fn measure_chip_label(text: &str, window: &Window) -> f32 {
    let text = SharedString::from(text);
    let run = TextRun {
        len: text.len(),
        font: font(crate::theme::ui_font_family()),
        ..Default::default()
    };
    f32::from(
        window
            .text_system()
            .shape_line(text, ui_px(crate::ui::CHIP_TEXT_SIZE), &[run], None)
            .width,
    )
}

/// Which of the row's demotable chips still show their labels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ComposerRowPlan {
    pub model_label: bool,
    pub traits_label: bool,
    pub interaction_label: bool,
    pub access_label: bool,
}

impl ComposerRowPlan {
    fn labelled() -> Self {
        Self {
            model_label: true,
            traits_label: true,
            interaction_label: true,
            access_label: true,
        }
    }
}

/// The richest plan `available` pixels pay for.
///
/// The model's share is measured before this runs, so a long name is charged
/// for in full up front and the walk below decides what gives way to it.
/// Each demotion is one label plus the gap that separated it from the icon,
/// and the walk stops as soon as the row fits — a chip that survives because
/// the row happened to have room keeps its label, rather than being demoted
/// on a fixed schedule.
pub(super) fn composer_row_plan(available: f32, widths: ComposerRowWidths) -> ComposerRowPlan {
    let mut plan = ComposerRowPlan::labelled();
    let mut over = widths.total() - available;
    for (label, demote) in [
        (widths.traits, &mut plan.traits_label),
        (widths.interaction, &mut plan.interaction_label),
        (widths.access, &mut plan.access_label),
        (widths.model, &mut plan.model_label),
    ] {
        if over <= 0.0 {
            break;
        }
        let Some(label) = label else { continue };
        *demote = false;
        // What the chip stops costing: the label and the gap that separated
        // it from the icon.
        over -= chip_width(Some(label)) - chip_width(None);
    }
    plan
}

impl Fintwind {
    // ── Permission ─────────────────────────────────────────────────────────

    pub(super) fn render_permission(&self, cx: &mut Context<Self>) -> Option<Div> {
        if let Some(input) = self.selected_runtime()?.pending_user_input.clone() {
            return Some(self.render_user_input(input, cx));
        }
        let permission = self.selected_runtime()?.pending_permission.as_ref()?;
        let theme = Theme::current(cx);
        let request_id = permission.request_id.clone();
        let mut buttons = div().flex().items_center().gap(px(8.0)).mt(px(10.0));
        for option in &permission.options {
            let request_id = request_id.clone();
            let option_id = option.id.clone();
            let allow = option.allow;
            buttons = buttons.child(
                div()
                    .id(SharedString::from(format!(
                        "permission-{}-{}",
                        permission.request_id, option.id
                    )))
                    .h(px(32.0))
                    .px(px(14.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(ui_px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(allow, |element| {
                        element
                            .bg(theme.inverse)
                            .text_color(theme.on_inverse)
                            .hover(|element| element.opacity(0.9))
                    })
                    .when(!allow, |element| {
                        element
                            .border_1()
                            .border_color(theme.border_strong)
                            .text_color(theme.text_secondary)
                            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
                    })
                    .active(|element| element.opacity(0.8))
                    .child(SharedString::from(option.label.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.respond_permission(request_id.clone(), option_id.clone(), cx);
                    })),
            );
        }
        Some(
            div().px(px(20.0)).pb(px(8.0)).child(
                div()
                    .w_full()
                    .max_w(px(crate::theme::transcript_width()))
                    .mx_auto()
                    .p(px(12.0))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_md()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon("icons/alert.svg", 13.0, theme.warning))
                            .child(
                                div()
                                    .text_size(ui_px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(SharedString::from(permission.title.clone())),
                            ),
                    )
                    .child(
                        div()
                            .id("permission-detail")
                            .mt(px(8.0))
                            .max_h(px(92.0))
                            .overflow_y_scroll()
                            .p(px(8.0))
                            .rounded(px(7.0))
                            .bg(theme.inset)
                            .font_family(crate::md::render::mono_family())
                            .text_size(ui_px(10.5))
                            .line_height(ui_px(16.0))
                            .text_color(theme.text_secondary)
                            .whitespace_normal()
                            .child(SharedString::from(permission.detail.clone())),
                    )
                    .child(buttons),
            ),
        )
    }

    fn render_user_input(&self, pending: PendingUserInput, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let Some(question) = pending.current_question().cloned() else {
            return div();
        };
        let selected = pending
            .selections
            .get(&question.id)
            .cloned()
            .unwrap_or_default();
        let has_custom = pending
            .custom_answers
            .get(&question.id)
            .is_some_and(|answer| !answer.trim().is_empty());
        let can_continue = has_custom || !selected.is_empty();
        let is_last = pending.question_index + 1 == pending.questions.len();
        let request_id = pending.request_id.clone();
        let question_index = pending.question_index;
        let mut options = div().mt(px(9.0)).flex().flex_col().gap(px(4.0));
        for (index, option) in question.options.iter().enumerate() {
            let is_selected = selected.iter().any(|answer| answer == &option.label);
            let click_label = option.label.clone();
            let key_label = option.label.clone();
            let focus = self.transcript_control_focus(
                format!("user-input-{request_id}-{question_index}-option-{index}"),
                cx,
            );
            options = options.child(
                div()
                    .id(SharedString::from(format!(
                        "user-input-{request_id}-{question_index}-option-{index}"
                    )))
                    .track_focus(&focus)
                    .tab_index(0)
                    .tab_stop(true)
                    .min_h(px(38.0))
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(if is_selected {
                        theme.accent.opacity(0.34)
                    } else {
                        theme.border.opacity(0.0)
                    })
                    .bg(if is_selected {
                        theme.accent.opacity(0.08)
                    } else {
                        theme.overlay
                    })
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_default()
                    .focus_visible(|style| style.border_color(theme.accent))
                    .when(!is_selected, |row| {
                        row.hover(|style| style.border_color(theme.border).bg(theme.overlay_strong))
                    })
                    .active(|style| style.opacity(0.85))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(SharedString::from(option.label.clone())),
                            )
                            .children(option.description.as_ref().map(|description| {
                                div()
                                    .mt(px(1.0))
                                    .text_size(ui_px(11.0))
                                    .line_height(ui_px(14.0))
                                    .text_color(theme.text_secondary)
                                    .whitespace_normal()
                                    .child(SharedString::from(description.clone()))
                            })),
                    )
                    .when(is_selected, |row| {
                        row.child(icon("icons/check.svg", 13.0, theme.accent))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_user_input_option(click_label.clone(), cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.select_user_input_option(key_label.clone(), cx);
                            cx.stop_propagation();
                        }
                    })),
            );
        }

        let next_focus = self.transcript_control_focus(
            format!("user-input-{request_id}-{question_index}-continue"),
            cx,
        );
        let back = (question_index > 0).then(|| {
            let focus = self.transcript_control_focus(
                format!("user-input-{request_id}-{question_index}-back"),
                cx,
            );
            div()
                .id(SharedString::from(format!(
                    "user-input-{request_id}-{question_index}-back"
                )))
                .track_focus(&focus)
                .tab_index(0)
                .tab_stop(true)
                .h(px(30.0))
                .px(px(9.0))
                .rounded(px(7.0))
                .flex()
                .items_center()
                .cursor_default()
                .text_size(ui_px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text_tertiary)
                .focus_visible(|style| style.border_1().border_color(theme.accent))
                .hover(|style| style.bg(theme.overlay).text_color(theme.text_secondary))
                .active(|style| style.opacity(0.8))
                .child(tr!("user_input.back"))
                .on_click(cx.listener(|this, _, _, cx| this.previous_user_input(cx)))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.previous_user_input(cx);
                        cx.stop_propagation();
                    }
                }))
        });
        let continue_button = div()
            .id(SharedString::from(format!(
                "user-input-{request_id}-{question_index}-continue"
            )))
            .track_focus(&next_focus)
            .tab_index(0)
            .tab_stop(can_continue)
            .h(px(30.0))
            .px(px(11.0))
            .rounded(px(7.0))
            .flex()
            .items_center()
            .cursor_default()
            .text_size(ui_px(12.0))
            .font_weight(FontWeight::SEMIBOLD)
            .bg(if can_continue {
                theme.inverse
            } else {
                theme.overlay
            })
            .text_color(if can_continue {
                theme.on_inverse
            } else {
                theme.text_ghost
            })
            .when(can_continue, |button| {
                button
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .hover(|style| style.opacity(0.9))
                    .active(|style| style.opacity(0.8))
                    .on_click(cx.listener(|this, _, _, cx| this.advance_user_input(cx)))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.advance_user_input(cx);
                            cx.stop_propagation();
                        }
                    }))
            })
            .child(if is_last {
                tr!("user_input.submit")
            } else {
                tr!("user_input.next")
            });

        let progress = (pending.questions.len() > 1).then(|| {
            div()
                .h(px(18.0))
                .px(px(6.0))
                .rounded(px(5.0))
                .bg(theme.overlay)
                .flex()
                .items_center()
                .text_size(ui_px(9.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text_tertiary)
                .child(tr!(
                    "user_input.progress",
                    current = question_index + 1,
                    total = pending.questions.len()
                ))
        });

        let toggle_focus =
            self.transcript_control_focus(format!("user-input-{request_id}-toggle"), cx);
        let toggle = div()
            .id(SharedString::from(format!(
                "user-input-{request_id}-toggle"
            )))
            .track_focus(&toggle_focus)
            .tab_index(0)
            .tab_stop(true)
            .w_full()
            .px(px(6.0))
            .py(px(2.0))
            .mx(px(-6.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay))
            .active(|style| style.opacity(0.85))
            .child(
                div()
                    .text_size(ui_px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(question.header.clone())),
            )
            .children(progress)
            .child(div().flex_1())
            .child(
                // The chevron rides in a fixed hit-area chip so the fold
                // control reads as a button rather than a stray glyph: quiet
                // at rest, raised on hover. The row around it stays the click
                // target — the chip only paints.
                div()
                    .id(SharedString::from(format!(
                        "user-input-{request_id}-toggle-glyph"
                    )))
                    .flex_none()
                    .w(px(20.0))
                    .h(px(20.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|style| style.bg(theme.overlay_strong))
                    .child(icon(
                        if pending.collapsed {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-up.svg"
                        },
                        12.0,
                        theme.text_tertiary,
                    )),
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_user_input_collapsed(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.toggle_user_input_collapsed(cx);
                    cx.stop_propagation();
                }
            }));

        div().flex_none().px(px(20.0)).pb(px(8.0)).child(
            div()
                .id(SharedString::from(format!("user-input-{request_id}")))
                .w_full()
                .max_w(px(crate::theme::transcript_width()))
                .mx_auto()
                .px(px(14.0))
                .pt(px(12.0))
                .pb(px(10.0))
                .rounded(px(13.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.composer)
                .tab_index(0)
                .tab_group()
                .tab_stop(false)
                .child(toggle)
                .when(!pending.collapsed, |card| {
                    card.child(
                        div()
                            .mt(px(5.0))
                            .text_size(ui_px(13.0))
                            .line_height(ui_px(18.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .whitespace_normal()
                            .child(SharedString::from(question.question.clone())),
                    )
                    .children((!question.options.is_empty()).then_some(options))
                    .child(
                        div()
                            .mt(px(if question.options.is_empty() {
                                9.0
                            } else {
                                4.0
                            }))
                            .h(px(36.0))
                            .px(px(10.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(if has_custom {
                                theme.accent.opacity(0.34)
                            } else {
                                theme.border.opacity(0.0)
                            })
                            .bg(if has_custom {
                                theme.accent.opacity(0.06)
                            } else {
                                theme.overlay
                            })
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .text_size(ui_px(12.5))
                            .line_height(ui_px(17.0))
                            .child(icon(
                                "icons/pencil.svg",
                                12.0,
                                if has_custom {
                                    theme.accent
                                } else {
                                    theme.text_ghost
                                },
                            ))
                            .child(self.user_input_answer.clone()),
                    )
                    .child(
                        div()
                            .mt(px(8.0))
                            .flex()
                            .items_center()
                            .children(back)
                            .child(div().flex_1())
                            .child(continue_button),
                    )
                }),
        )
    }

    // ── Composer ───────────────────────────────────────────────────────────

    pub(super) fn render_provider_model_control(
        &self,
        label: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let provider = self.selected_model_provider_label();
        let selected_model = session.and_then(|session| self.model_for_session(session));
        let selected_model_name = self.model_display_name(selected_model);
        // The mark names the model's company, so a router serving Grok still
        // reads as Grok; unknown models keep the provider's letter glyph.
        let model_icon = model_icon(
            selected_model.unwrap_or(""),
            &selected_model_name,
            &provider,
        );
        let picker_enabled = session.is_some_and(|session| session.can_choose_model());

        if !picker_enabled {
            // A session that cannot change its model draws the name as
            // plain text rather than as a chip, so the demoted state is
            // built here too: the logo alone, with the name on hover.
            // The id is what makes the div interactive, and a tooltip needs
            // that to attach.
            let tooltip = (!label).then(|| Tooltip::text(selected_model_name.clone()));
            return div()
                .id("composer-provider-model-static")
                .h(px(24.0))
                .px(px(7.0))
                .flex()
                .items_center()
                .gap(px(if label { 6.0 } else { 0.0 }))
                .child(icon(
                    model_icon,
                    10.5,
                    provider_color(&theme, &provider).opacity(0.9),
                ))
                .when(label, |element| {
                    element.child(
                        div()
                            .max_w(px(MODEL_CHIP_MAX_LABEL))
                            .truncate()
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(selected_model_name.clone())),
                    )
                })
                .when_some(tooltip, |element, tooltip| element.tooltip(tooltip))
                .into_any_element();
        }

        let search_query = self.model_search.read(cx).content().to_owned();
        let normalized_query = search_query.trim().to_ascii_lowercase();
        let searching = !normalized_query.is_empty();
        let selected_tab = self.model_picker_tab.clone();
        let selected_model = selected_model.map(str::to_owned);
        let probes = self.probes.clone();
        let pending_discoveries = self.provider_model_discoveries_pending.clone();
        let provider_directory = self.provider_directory();
        let provider_detection_pending = self.provider_detection_remaining > 0;
        let favorites = self.state.favorite_models.clone();
        let weak = cx.entity().downgrade();
        let search = self.model_search.clone();
        let search_focus = search.read(cx).focus_handle(cx);

        let handle = {
            let reset_weak = weak.clone();
            let reset_search = search.clone();
            let picker_focus = search_focus.clone();
            self.menu_handle_with(MODEL_PICKER_MENU_ID, cx, move |open, window, cx| {
                let _ = reset_weak.update(cx, |this, cx| {
                    if open {
                        this.model_picker_tab = this.selected_model_picker_tab();
                        // Opening re-runs the tab's catalog discovery so models
                        // authored since launch appear without a restart.
                        this.refresh_provider_model_discovery();
                        this.model_picker_highlight = None;
                        reset_search.update(cx, |search, cx| search.clear(cx));
                        this.reveal_selected_picker_model();
                    } else {
                        let focus_handle = this.composer.read(cx).focus();
                        window.focus(&focus_handle, cx);
                    }
                    cx.notify();
                });
                if open {
                    // The panel is deferred, so its input joins the dispatch
                    // tree only after the deferred draw — same two-frame wait
                    // the menus need before they can take focus. The reveal is
                    // re-issued here too: a parked scroll request resolves
                    // against the viewport bounds of the *previous* paint, so
                    // on the container's first-ever paint it reads a zeroed
                    // viewport, lands wrong, and is consumed. By this frame
                    // the panel has painted real bounds to resolve against.
                    let picker_focus = picker_focus.clone();
                    let reveal_weak = reset_weak.clone();
                    window.on_next_frame(move |window, _| {
                        window.on_next_frame(move |window, cx| {
                            window.focus(&picker_focus, cx);
                            let _ = reveal_weak.update(cx, |this, _| {
                                this.reveal_selected_picker_model();
                            });
                        });
                    });
                }
            })
        };

        // Only while the panel is open: this clones the provider's model list,
        // and the closed picker is on the composer's every frame.
        // Built out here rather than in the body so the key handler and the
        // rendered rows index one ordering and cannot disagree about what
        // `enter` selects.
        let available_models = Rc::new(if handle.is_open() {
            visible_picker_models(&probes, &favorites, selected_tab.clone(), &normalized_query)
        } else {
            Vec::new()
        });
        let highlight = self
            .model_picker_highlight
            .filter(|index| *index < available_models.len());
        let scroll = self.model_picker_scroll.clone();
        let scrollbar_state = self.model_picker_scrollbar.clone();

        // A provider's display name is not bounded, and the row's budget
        // caps what it is willing to pay for this chip at the same width.
        // Without the cap here the budget would read a long name as
        // affordable and then let it push the chips beside it off the row.
        // A demoted chip is the company logo alone, which names the company
        // but not the model — so the name moves into a tooltip.
        let tooltip = (!label).then(|| selected_model_name.clone());
        let chip = MenuChip::new("composer-provider-model")
            .icon(model_icon, provider_color(&theme, &provider).opacity(0.9))
            .label(selected_model_name)
            .caret(false)
            .max_label_width(MODEL_CHIP_MAX_LABEL)
            .icon_only(!label)
            .when_some(tooltip, |chip, label| chip.tooltip(label))
            .selected(handle.is_open());
        popover(
            chip,
            &handle,
            MenuAlign::AboveLeft,
            move |popover, _window, _cx| {
                let popover = popover.clone();
                let available_models = available_models.clone();
                let picker_tabs = visible_picker_tabs(&probes);

                let mut sidebar = div()
                    .w(px(50.0))
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.0))
                    .p(px(5.0))
                    .rounded_tl(px(12.0))
                    .rounded_bl(px(12.0))
                    .bg(theme.canvas)
                    .border_r_1()
                    .border_color(theme.border);

                let favorites_selected = selected_tab == ModelPickerTab::Favorites && !searching;
                let favorite_weak = weak.clone();
                sidebar = sidebar
                    .child(
                        div()
                            .id("model-tab-favorites")
                            .w(px(40.0))
                            .h(px(40.0))
                            .rounded(px(8.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_default()
                            .when(favorites_selected, |element| {
                                element.bg(theme.accent.opacity(0.12))
                            })
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.bg(theme.overlay_strong))
                            .child(icon(
                                "icons/star.svg",
                                18.0,
                                if favorites_selected {
                                    theme.accent
                                } else {
                                    theme.text_tertiary
                                },
                            ))
                            .on_click(move |_, _, cx| {
                                let _ = favorite_weak.update(cx, |this, cx| {
                                    this.select_model_picker_tab(ModelPickerTab::Favorites, cx);
                                });
                            }),
                    )
                    .child(div().w(px(34.0)).h(px(1.0)).my(px(3.0)).bg(theme.border));

                let provider_tabs = picker_tabs
                    .iter()
                    .filter_map(|tab| match tab {
                        ModelPickerTab::Provider(label) => Some((tab.clone(), label.clone())),
                        ModelPickerTab::Favorites => None,
                    })
                    .collect::<Vec<_>>();
                let provider_weak = weak.clone();
                let mut provider_sidebar = div()
                    .id("model-provider-tabs")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll();
                for (provider_tab, provider_label) in provider_tabs {
                    let is_selected = selected_tab == provider_tab && !searching;
                    let icon_path = provider_icon(&provider_label);
                    let tooltip = provider_label.clone();
                    let select_tab = provider_tab.clone();
                    let provider_weak = provider_weak.clone();
                    provider_sidebar = provider_sidebar.child(
                        div()
                            .id(SharedString::from(format!(
                                "model-tab-provider-{}",
                                provider_label.to_ascii_lowercase()
                            )))
                            .w(px(40.0))
                            .h(px(40.0))
                            .flex_none()
                            .rounded(px(8.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_default()
                            .when(is_selected, |element| {
                                element.bg(theme.accent.opacity(0.12))
                            })
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.bg(theme.overlay_strong))
                            .tooltip(Tooltip::text(tooltip))
                            .on_click(move |_, _, cx| {
                                let _ = provider_weak.update(cx, |this, cx| {
                                    this.select_model_picker_tab(select_tab.clone(), cx);
                                });
                            })
                            .child(icon(
                                icon_path,
                                18.0,
                                if is_selected {
                                    theme.accent
                                } else {
                                    theme.text.opacity(0.82)
                                },
                            )),
                    );
                }
                sidebar = sidebar.child(provider_sidebar);

                let search_input = div()
                    .h(px(54.0))
                    .px(px(12.0))
                    .pt(px(10.0))
                    .pb(px(8.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .h(px(36.0))
                            .px(px(10.0))
                            .rounded(px(9.0))
                            .bg(theme.raised)
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon("icons/search.svg", 15.0, theme.text_secondary))
                            .child(div().flex_1().min_w_0().child(search.clone())),
                    );

                let mut rows = div()
                    .id("model-picker-list")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .p(px(9.0));
                if available_models.is_empty() {
                    let label = if searching {
                        tr!("models.none_found")
                    } else if selected_tab == ModelPickerTab::Favorites {
                        tr!("models.favorite_hint")
                    } else if provider_detection_pending
                        || pending_discoveries.contains(&provider_directory)
                    {
                        tr!("models.loading")
                    } else if probes.iter().any(|probe| probe.installed)
                        && probes.iter().all(|probe| probe.models.is_empty())
                    {
                        tr!("providers.no_available_models")
                    } else {
                        tr!("models.none_reported")
                    };
                    rows = rows.child(
                        div()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(ui_px(11.5))
                            .text_color(theme.text_muted)
                            .child(label),
                    );
                }

                for (row_index, model) in available_models.iter().enumerate() {
                    let is_selected = selected_model.as_deref() == Some(model.id.as_str());
                    let is_highlighted = highlight == Some(row_index);
                    let is_favorite = favorites.iter().any(|favorite| favorite.model == model.id);
                    let model_id = model.id.clone();
                    let select_weak = weak.clone();
                    let select_popover = popover.clone();
                    let favorite_model_id = model.id.clone();
                    let favorite_weak = weak.clone();
                    rows = rows.child(
                        div()
                            .id(SharedString::from(format!("model-row-{}", model.id)))
                            .h(px(60.0))
                            .px(px(12.0))
                            .rounded(px(9.0))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .cursor_default()
                            // Reserved on every row so highlighting one cannot
                            // resize it and shift the list by a pixel.
                            .border_1()
                            .border_color(if is_selected {
                                theme.accent.opacity(0.34)
                            } else {
                                gpui::transparent_black()
                            })
                            .when(is_selected, |element| {
                                element.bg(theme.accent.opacity(0.10))
                            })
                            // The keyboard cursor reads as a ring rather than a
                            // fill, so it stays legible on the current model's
                            // already-filled row.
                            .when(is_highlighted, |element| {
                                element
                                    .bg(theme.accent.opacity(0.14))
                                    .border_color(theme.accent)
                            })
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.opacity(0.85))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(ui_px(13.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text)
                                            .child(SharedString::from(model.name.clone())),
                                    )
                                    .child(
                                        div()
                                            .mt(px(4.0))
                                            .truncate()
                                            .text_size(ui_px(11.5))
                                            .text_color(theme.text_tertiary)
                                            .child(SharedString::from(model.id.clone())),
                                    ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("favorite-model-{}", model.id)))
                                    .w(px(30.0))
                                    .h(px(30.0))
                                    .rounded(px(7.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|element| element.bg(theme.overlay_strong))
                                    .active(|element| element.opacity(0.8))
                                    .child(icon(
                                        if is_favorite {
                                            "icons/star-filled.svg"
                                        } else {
                                            "icons/star.svg"
                                        },
                                        15.0,
                                        if is_favorite {
                                            theme.favorite
                                        } else {
                                            theme.text_ghost
                                        },
                                    ))
                                    .on_click(move |_, _, cx| {
                                        cx.stop_propagation();
                                        let _ = favorite_weak.update(cx, |this, cx| {
                                            this.toggle_favorite_model(
                                                favorite_model_id.clone(),
                                                cx,
                                            );
                                        });
                                    }),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = select_weak.update(cx, |this, cx| {
                                    this.choose_model(model_id.clone(), cx);
                                });
                                select_popover.close(window, cx);
                            }),
                    );
                }

                let next_models = available_models.clone();
                let previous_models = available_models.clone();
                let confirm_models = available_models.clone();
                let next_weak = weak.clone();
                let previous_weak = weak.clone();
                let next_tab_weak = weak.clone();
                let previous_tab_weak = weak.clone();
                let confirm_weak = weak.clone();
                let confirm_popover = popover.clone();
                div()
                    .w(px(460.0))
                    .h(px(390.0))
                    .rounded(px(13.0))
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_lg()
                    .flex()
                    // The filter field keeps focus and the selected row is only
                    // drawn, never focused — the same split Zed's picker uses.
                    // These arrive as actions bound to `FintwindMenu > ComposerInput`,
                    // which is the only way to claim a key out from under a
                    // focused text field.
                    .on_action(move |_: &SelectNextEntry, _, cx| {
                        let _ = next_weak.update(cx, |this, cx| {
                            this.move_model_picker_highlight("down", &next_models, cx);
                        });
                    })
                    .on_action(move |_: &SelectPreviousEntry, _, cx| {
                        let _ = previous_weak.update(cx, |this, cx| {
                            this.move_model_picker_highlight("up", &previous_models, cx);
                        });
                    })
                    .on_action(move |_: &SelectNextTab, _, cx| {
                        let _ = next_tab_weak.update(cx, |this, cx| {
                            this.cycle_model_picker_tab("down", cx);
                        });
                    })
                    .on_action(move |_: &SelectPreviousTab, _, cx| {
                        let _ = previous_tab_weak.update(cx, |this, cx| {
                            this.cycle_model_picker_tab("up", cx);
                        });
                    })
                    .on_action(move |_: &ConfirmEntry, window, cx| {
                        let _ = confirm_weak.update(cx, |this, cx| {
                            this.choose_highlighted_model(&confirm_models, cx);
                        });
                        confirm_popover.close(window, cx);
                        window.refresh();
                    })
                    .child(sidebar)
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .rounded_tr(px(12.0))
                            .rounded_br(px(12.0))
                            .bg(theme.surface)
                            .child(search_input)
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .relative()
                                    .child(rows)
                                    .child(scrollbar::vertical(&scroll, &scrollbar_state)),
                            ),
                    )
                    .into_any_element()
            },
        )
    }

    /// Move the picker's drawn selection. Nothing is focused: the filter field
    /// keeps focus so typing continues to narrow the list.
    fn move_model_picker_highlight(
        &mut self,
        key: &str,
        models: &[ProviderModel],
        cx: &mut Context<Self>,
    ) {
        let current = self
            .model_picker_highlight
            .filter(|index| *index < models.len());
        let Some(next) = next_picker_highlight(current, models.len(), key) else {
            return;
        };
        self.model_picker_highlight = Some(next);
        self.model_picker_scroll.scroll_to_item(next);
        cx.notify();
    }

    /// Step the sidebar rail to the adjacent usable tab, wrapping at both
    /// ends. `tab`/`shift-tab` land here from under the focused filter field,
    /// the same route the arrows take. A live query hides which tab is
    /// selected and searches across all of them, so cycling waits until the
    /// field is cleared.
    fn cycle_model_picker_tab(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.model_search.read(cx).content().trim().is_empty() {
            return;
        }
        let tabs = visible_picker_tabs(&self.probes);
        let current = tabs.iter().position(|tab| *tab == self.model_picker_tab);
        let Some(next) = next_picker_highlight(current, tabs.len(), key) else {
            return;
        };
        self.select_model_picker_tab(tabs[next].clone(), cx);
    }

    /// Bring the current model's row into view whenever the picker shows the
    /// unfiltered list — on open, on a cleared query, and on tab switches.
    ///
    /// The request parks in the scroll handle until the row list next paints,
    /// so it may be issued from the open toggle before the deferred panel
    /// exists, and a tab whose models are still loading reveals the row once
    /// they arrive. Without a row to reveal it falls back to the top, so a
    /// scroll offset from an earlier open never leaks into a fresh list.
    pub(super) fn reveal_selected_picker_model(&self) {
        let session = self.selected_session();
        let selected_model = session.and_then(|session| self.model_for_session(session));
        let models = visible_picker_models(
            &self.probes,
            &self.state.favorite_models,
            self.model_picker_tab.clone(),
            "",
        );
        let index = models
            .iter()
            .position(|model| selected_model == Some(model.id.as_str()))
            .unwrap_or(0);
        self.model_picker_scroll.scroll_to_item(index);
    }

    pub(super) fn selected_model_picker_tab(&self) -> ModelPickerTab {
        ModelPickerTab::Provider(self.selected_model_provider_label())
    }

    /// Display label of the provider backing the selected session's model —
    /// the key the model chip's icon and the picker's active tab share, so
    /// both always describe the same provider.
    pub(super) fn selected_model_provider_label(&self) -> String {
        model_picker_provider_label(
            self.selected_session()
                .and_then(|session| self.model_metadata_for_session(session))
                .and_then(|model| model.sub_provider.as_deref()),
        )
    }

    /// Take the row the selection is on, defaulting to the first so `enter`
    /// works the moment the panel opens.
    fn choose_highlighted_model(&mut self, models: &[ProviderModel], cx: &mut Context<Self>) {
        let Some(model) = models.get(self.model_picker_highlight.unwrap_or(0)) else {
            return;
        };
        let model_id = model.id.clone();
        self.choose_model(model_id, cx);
    }

    /// The traits the selected session's model offers.
    ///
    /// Resolved once per frame and handed to both the width budget and the
    /// chip, so the two cannot read the same session as offering different
    /// things — and so the answer is not computed twice to reach the same
    /// conclusion.
    fn model_traits_for_session(&self) -> Option<ModelTraits<'_>> {
        let session = self.selected_session()?;
        let model = self.model_metadata_for_session(session)?;
        ModelTraits::resolve(session, model)
    }

    fn render_model_traits_control(
        &self,
        traits: Option<&ModelTraits<'_>>,
        label: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::current(cx);
        let traits = traits?;
        let selected_effort = traits.effort.clone();
        let selected_tier = traits.tier.clone();
        let selected_window = traits.window.clone();
        let fast = traits.fast;
        let trigger_label = traits.label.clone();
        let model = traits.model;
        let reasoning_efforts = model.reasoning_efforts.clone();
        let default_effort = model.default_reasoning_effort.clone();
        let service_tiers = model.service_tiers.clone();
        let context_windows = model.context_windows.clone();
        let default_window = model.default_context_window.clone();
        let default_tier = model
            .default_service_tier
            .clone()
            .unwrap_or_else(|| "default".to_owned());
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("model-traits", cx);
        // The demoted chip is the zap alone, and a zap does not say which
        // effort it stands for — so the label moves into a tooltip.
        let tooltip = (!label).then(|| trigger_label.clone());
        let chip = MenuChip::new("model-traits")
            .when(fast, |trigger| {
                trigger.icon("icons/zap.svg", theme.text_secondary)
            })
            .label(trigger_label)
            .caret(false)
            .icon_only(!label)
            .when_some(tooltip, |chip, label| chip.tooltip(label))
            .selected(handle.is_open());
        Some(dropdown_menu(
            chip,
            "model-traits-menu",
            &handle,
            MenuAlign::AboveLeft,
            move |_| {
                let mut items = Vec::new();
                if !reasoning_efforts.is_empty() {
                    items.push(MenuItem::Header(tr!("models.reasoning").into()));
                    for option in reasoning_efforts.clone() {
                        let weak = weak.clone();
                        let effort = option.id;
                        let is_default = default_effort.as_deref() == Some(effort.as_str());
                        let selected = selected_effort.as_deref() == Some(effort.as_str());
                        items.push(
                            traits_choice(theme, option.label, is_default, selected).on_click(
                                move |_, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.set_reasoning_effort(effort.clone(), cx);
                                    });
                                },
                            ),
                        );
                    }
                }
                if !service_tiers.is_empty() {
                    if !reasoning_efforts.is_empty() {
                        items.push(MenuItem::Separator);
                    }
                    items.push(MenuItem::Header(tr!("models.service_tier").into()));
                    let weak_standard = weak.clone();
                    items.push(
                        traits_choice(
                            theme,
                            tr!("models.standard"),
                            default_tier == "default",
                            selected_tier == "default",
                        )
                        .on_click(move |_, cx| {
                            let _ = weak_standard.update(cx, |this, cx| {
                                this.set_service_tier("default".to_owned(), cx);
                            });
                        }),
                    );
                    for option in service_tiers.clone() {
                        let weak = weak.clone();
                        let tier = option.id;
                        let is_default = default_tier == tier;
                        let selected = selected_tier == tier;
                        items.push(
                            traits_choice(theme, option.label, is_default, selected).on_click(
                                move |_, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.set_service_tier(tier.clone(), cx);
                                    });
                                },
                            ),
                        );
                    }
                }
                if !context_windows.is_empty() {
                    if !reasoning_efforts.is_empty() || !service_tiers.is_empty() {
                        items.push(MenuItem::Separator);
                    }
                    items.push(MenuItem::Header(tr!("models.context_window").into()));
                    for option in context_windows.clone() {
                        let weak = weak.clone();
                        let window = option.id;
                        let is_default = default_window.as_deref() == Some(window.as_str());
                        let selected = selected_window.as_deref() == Some(window.as_str());
                        items.push(
                            traits_choice(theme, option.label, is_default, selected).on_click(
                                move |_, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.set_context_window(window.clone(), cx);
                                    });
                                },
                            ),
                        );
                    }
                }
                items
            },
        ))
    }

    pub(super) fn render_access_control(&self, label: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let selected_mode = self
            .selected_session()
            .map(|session| session.runtime_mode.access())
            .unwrap_or_default();
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("runtime-mode", cx);
        let mode_label = selected_mode.label();
        // The demoted chip shows only its lock, and a lock does not say
        // which of the three levels is in force — so the label moves into a
        // tooltip rather than off the row.
        let tooltip = (!label).then(|| mode_label.clone());
        let chip = MenuChip::new("runtime-mode")
            .icon(selected_mode.icon(), theme.text_tertiary)
            .label(mode_label)
            .caret(false)
            .icon_only(!label)
            .when_some(tooltip, |chip, label| chip.tooltip(label))
            .selected(handle.is_open());
        dropdown_menu(
            chip,
            "runtime-mode-menu",
            &handle,
            MenuAlign::AboveLeft,
            move |_| {
                RuntimeMode::ACCESS_OPTIONS
                    .into_iter()
                    .map(|option| {
                        let weak = weak.clone();
                        let selected = option == selected_mode;
                        MenuItem::custom(move |_, _| {
                            div()
                                .w(px(288.0))
                                .py(px(5.0))
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .child(icon(option.icon(), 15.0, theme.text_tertiary))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .w_full()
                                                .truncate()
                                                .text_size(ui_px(12.5))
                                                .font_weight(if selected {
                                                    FontWeight::SEMIBOLD
                                                } else {
                                                    FontWeight::MEDIUM
                                                })
                                                .text_color(theme.text)
                                                .child(option.label()),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .mt(px(2.0))
                                                .text_size(ui_px(11.0))
                                                .line_height(ui_px(15.0))
                                                .whitespace_normal()
                                                .text_color(theme.text_tertiary)
                                                .child(option.description()),
                                        ),
                                )
                                .when(selected, |element| {
                                    element.child(icon(
                                        "icons/check.svg",
                                        12.0,
                                        theme.text_tertiary,
                                    ))
                                })
                                .into_any_element()
                        })
                        .on_click(move |_, cx| {
                            let _ = weak.update(cx, |this, cx| this.set_runtime_mode(option, cx));
                        })
                    })
                    .collect()
            },
        )
    }

    pub(super) fn render_interaction_mode_control(
        &self,
        label: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let mode = self
            .selected_session()
            .map(|session| session.interaction_mode)
            .unwrap_or_default();
        let next_mode = if mode == InteractionMode::Plan {
            InteractionMode::Build
        } else {
            InteractionMode::Plan
        };
        let weak = cx.entity().downgrade();
        let plan = mode == InteractionMode::Plan;
        let mode_label = mode.label();
        // A demoted chip is a list or a wrench alone, and neither says
        // whether the agent is about to plan or build — so the label moves
        // into a tooltip rather than off the row.
        let tooltip = (!label).then(|| Tooltip::text(mode_label.clone()));
        div()
            .id("interaction-mode")
            .h(px(28.0))
            .px(px(9.0))
            .rounded(px(7.0))
            .flex()
            .items_center()
            // Matches the chip geometry `chip_width` prices, so a demoted
            // control costs the budget exactly what it paints.
            .gap(px(if label { 7.0 } else { 0.0 }))
            .cursor_default()
            .text_size(ui_px(12.5))
            .line_height(ui_px(16.0))
            .text_color(if plan {
                theme.accent
            } else {
                theme.text_secondary
            })
            .child(icon(
                if plan {
                    "icons/list.svg"
                } else {
                    "icons/wrench.svg"
                },
                12.0,
                if plan {
                    theme.accent
                } else {
                    theme.text_tertiary
                },
            ))
            .when(label, |element| element.child(mode_label))
            .when_some(tooltip, |element, tooltip| element.tooltip(tooltip))
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .on_click(move |_, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.set_interaction_mode(next_mode, cx);
                });
            })
            .into_any_element()
    }

    /// Stage files dropped onto the chat column as composer attachment chips. The bytes
    /// are copied into the daemon attachment store; submit sends that path
    /// as a prompt `files` URI, not as text.
    pub(super) fn stage_dropped_files(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.stage_attachment_paths(paths.paths(), cx) {
            return;
        }
        let focus = self.composer.read(cx).focus();
        window.focus(&focus, cx);
    }

    fn stage_attachment_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) -> bool {
        if paths.is_empty() {
            return false;
        }
        let paths = paths.to_vec();
        let daemon = self.daemon.clone();
        let draft_owner = self.selected_composer_draft_key();
        cx.spawn(async move |fintwind, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut stored = Vec::with_capacity(paths.len());
                    for source_path in paths {
                        let (name, upload, image_bytes) =
                            attachment_upload_from_path(&source_path)?;
                        let is_image = image_bytes.is_some();
                        let preview_image = image_bytes.and_then(|bytes| {
                            image_preview::image_format_for_name(&name)
                                .map(|format| Arc::new(gpui::Image::from_bytes(format, bytes)))
                        });
                        let response = daemon.client().request(
                            Uuid::nil(),
                            Uuid::nil(),
                            fintwind_client::Command::ImportAttachment { name, upload },
                        )?;
                        let fintwind_client::ResponsePayload::AttachmentStored { attachment } =
                            response
                        else {
                            anyhow::bail!("the daemon returned an invalid attachment response");
                        };
                        stored.push((attachment, preview_image, is_image));
                    }
                    Ok::<_, anyhow::Error>(stored)
                })
                .await;
            let _ = fintwind.update(cx, |fintwind, cx| match result {
                Ok(stored) => {
                    if fintwind.selected_composer_draft_key() != draft_owner {
                        return;
                    }
                    let mut changed = false;
                    for (attachment, preview_image, is_image) in stored {
                        changed |= fintwind.stage_daemon_attachment(
                            attachment.path,
                            attachment.name,
                            attachment.is_dir,
                            is_image,
                            attachment.reference,
                            preview_image,
                        );
                    }
                    if changed {
                        fintwind.schedule_composer_draft_save(cx);
                        cx.notify();
                    }
                }
                Err(error) => {
                    fintwind.show_toast(error.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
        true
    }

    fn stage_daemon_attachment(
        &mut self,
        path: PathBuf,
        name: String,
        is_dir: bool,
        is_image: bool,
        reference: String,
        client_preview_image: Option<Arc<gpui::Image>>,
    ) -> bool {
        if self.composer_attachments.iter().any(|attachment| {
            attachment.path == path
                || attachment.blob_reference.as_deref() == Some(reference.as_str())
        }) {
            return false;
        }
        let mut mention = path.display().to_string();
        if is_dir && !mention.ends_with('/') {
            mention.push('/');
        }
        self.composer_attachments.push(ComposerAttachment {
            path,
            client_preview_image,
            mention,
            name: SharedString::from(name),
            is_dir,
            is_image,
            blob_reference: Some(reference),
        });
        true
    }

    /// Stage the clipboard's primary image/file representation. On-disk paths
    /// reuse drop handling immediately; raw image bytes are copied into Fintwind's
    /// durable blob store on the background executor before their chip appears.
    pub(super) fn stage_pasted_attachments(
        &mut self,
        entries: Vec<ClipboardEntry>,
        cx: &mut Context<Self>,
    ) {
        let mut paths = Vec::new();
        let mut images = Vec::new();
        for entry in entries {
            match entry {
                ClipboardEntry::Image(image) if !image.bytes.is_empty() => images.push(image),
                ClipboardEntry::ExternalPaths(external) => {
                    paths.extend(external.paths().iter().cloned())
                }
                ClipboardEntry::String(_) | ClipboardEntry::Image(_) => {}
            }
        }
        self.stage_attachment_paths(&paths, cx);
        if images.is_empty() {
            return;
        }

        let daemon = self.daemon.clone();
        let draft_owner = self.selected_composer_draft_key();
        cx.spawn(async move |fintwind, cx| {
            let stored = cx
                .background_executor()
                .spawn(async move {
                    let image_count = images.len();
                    images
                        .into_iter()
                        .enumerate()
                        .map(|(index, image)| {
                            let preview_image = Arc::new(image);
                            let bytes = preview_image.bytes.clone();
                            if bytes.len() as u64 > MAX_PROMPT_FILE_BYTES {
                                return Err("attachment is larger than 20 MB".into());
                            }
                            let response = daemon
                                .client()
                                .request(
                                    Uuid::nil(),
                                    Uuid::nil(),
                                    fintwind_client::Command::StoreBlob {
                                        mime_type: preview_image.format.mime_type().to_owned(),
                                        bytes,
                                    },
                                )
                                .map_err(|error| error.to_string())?;
                            let fintwind_client::ResponsePayload::BlobStored { reference, path } =
                                response
                            else {
                                return Err("the daemon returned an invalid blob response".into());
                            };
                            let extension = path
                                .extension()
                                .and_then(|extension| extension.to_str())
                                .unwrap_or("png");
                            let name = if image_count == 1 {
                                format!("image.{extension}")
                            } else {
                                format!("image-{}.{extension}", index + 1)
                            };
                            Ok::<_, String>((path, name, reference, preview_image))
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .await;
            let _ = fintwind.update(cx, |fintwind, cx| match stored {
                Ok(stored) => {
                    if fintwind.selected_composer_draft_key() != draft_owner {
                        return;
                    }
                    let mut staged = false;
                    for (path, name, reference, preview_image) in stored {
                        staged |= fintwind.stage_daemon_attachment(
                            path,
                            name,
                            false,
                            true,
                            reference,
                            Some(preview_image),
                        );
                    }
                    if staged {
                        fintwind.schedule_composer_draft_save(cx);
                        cx.notify();
                    }
                }
                Err(error) => {
                    fintwind.show_toast(tr!("errors.store_pasted_image", error = error));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The text and attachments accepted from the composer. The provider
    /// receives the typed text unchanged; each chip is a prompt `files` entry
    /// built from the daemon path, not an `@` mention appended to the text.
    pub(super) fn submission_with_attachments(
        &mut self,
        prompt: &str,
        cx: &mut Context<Self>,
    ) -> Option<ComposerSubmission> {
        for attachment in &self.composer_attachments {
            if let (Some(reference), Some(image)) = (
                attachment.blob_reference.as_ref(),
                attachment.client_preview_image.as_ref(),
            ) {
                self.remote_images
                    .borrow_mut()
                    .insert(reference.clone(), RemoteImageState::Ready(image.clone()));
            }
        }
        let attachments = self
            .composer_attachments
            .drain(..)
            .map(MessageAttachment::from)
            .collect::<Vec<_>>();
        let Some(prompt) = submission_text(prompt, attachments.len()) else {
            return None;
        };
        self.discard_current_composer_draft(cx);
        Some(ComposerSubmission {
            prompt,
            display_content: None,
            attachments,
        })
    }

    pub(super) fn restore_composer_submission(
        &mut self,
        submission: ComposerSubmission,
        cx: &mut Context<Self>,
    ) {
        self.composer_attachments = submission
            .attachments
            .into_iter()
            .map(ComposerAttachment::from)
            .collect();
        let content = submission.display_content.unwrap_or(submission.prompt);
        self.composer
            .update(cx, |input, cx| input.set_content(content, cx));
        self.schedule_composer_draft_save(cx);
        cx.notify();
    }

    /// The staged-attachment chips above the input: a thumbnail tile per
    /// image, a file-type icon and basename for everything else, each with a
    /// floating remove button — T3 Code's attachment row in graphite.
    fn render_composer_attachments(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let mut row = div()
            .px(px(14.0))
            .pt(px(2.0))
            .pb(px(8.0))
            .flex()
            .flex_wrap()
            .gap(px(8.0));
        for (index, attachment) in self.composer_attachments.iter().enumerate() {
            let menu = self.menu_handle(format!("composer-attachment-{index}-menu"), cx);
            let icon_path = if attachment.is_dir {
                "icons/folder.svg"
            } else {
                super::right_panel::file_icon_for_path(&attachment.mention)
            };
            let mut tile = div()
                .id(SharedString::from(format!("composer-attachment-{index}")))
                .relative()
                .w(px(64.0))
                .h(px(64.0))
                .rounded(px(8.0))
                .overflow_hidden()
                .border_1()
                .border_color(theme.border)
                .bg(theme.inset)
                .track_focus(menu.trigger_focus_handle())
                .tab_index(0)
                .focus_visible(|style| style.border_color(theme.accent))
                .tooltip(Tooltip::text(attachment.mention.clone()));
            let attachment_image = attachment.client_preview_image.clone().or_else(|| {
                attachment
                    .is_image
                    .then(|| {
                        attachment.blob_reference.as_deref().and_then(|reference| {
                            self.image_for_reference(
                                reference,
                                Some(&attachment.path),
                                Some(attachment.name.as_ref()),
                                cx,
                            )
                        })
                    })
                    .flatten()
            });
            let can_reveal = !self.daemon.is_remote();
            if attachment.is_image {
                if let Some(attachment_image) = attachment_image.as_ref() {
                    let preview_image = attachment_image.clone();
                    let preview_name = attachment.name.clone();
                    tile = tile.child(
                        div()
                            .id(SharedString::from(format!(
                                "composer-attachment-{index}-preview"
                            )))
                            .size_full()
                            .cursor_default()
                            .hover(|element| element.opacity(0.85))
                            .active(|element| element.opacity(0.72))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_image_preview(
                                    preview_image.clone(),
                                    preview_name.clone(),
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                            }))
                            .child(
                                img(attachment_image.clone())
                                    .size_full()
                                    .object_fit(ObjectFit::Cover),
                            ),
                    );
                } else {
                    tile = tile.child(
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("icons/file-types/image.svg", 16.0, theme.text_ghost)),
                    );
                }
            } else {
                tile = tile.child(
                    div()
                        .size_full()
                        .px(px(5.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(5.0))
                        .child(icon(icon_path, 16.0, theme.text_tertiary))
                        .child(
                            div().w_full().flex().justify_center().child(
                                div()
                                    .max_w_full()
                                    .truncate()
                                    .text_size(ui_px(8.5))
                                    .text_color(theme.text_tertiary)
                                    .child(attachment.name.clone()),
                            ),
                        ),
                );
            }
            let key_menu = menu.clone();
            let key_image = attachment_image.clone();
            let key_name = attachment.name.clone();
            let is_image = attachment.is_image;
            tile = tile.on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if is_image
                    && matches!(key, "enter" | "space")
                    && let Some(key_image) = key_image.as_ref()
                {
                    this.open_image_preview(key_image.clone(), key_name.clone(), window, cx);
                    cx.stop_propagation();
                } else if key == "f10" && event.keystroke.modifiers.shift {
                    key_menu.open_context_menu(window, cx);
                    cx.stop_propagation();
                }
            }));
            let tile = tile.child(
                div()
                    .id(SharedString::from(format!(
                        "composer-attachment-remove-{index}"
                    )))
                    .absolute()
                    .top(px(2.0))
                    .right(px(2.0))
                    .w(px(24.0))
                    .h(px(24.0))
                    .tab_index(0)
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .bg(theme.canvas.opacity(0.8))
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .hover(|element| element.bg(theme.canvas.opacity(0.95)))
                    .active(|element| element.opacity(0.8))
                    .child(icon("icons/x.svg", 10.0, theme.text_secondary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if index < this.composer_attachments.len() {
                            this.composer_attachments.remove(index);
                            this.schedule_composer_draft_save(cx);
                            cx.notify();
                        }
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            if index < this.composer_attachments.len() {
                                this.composer_attachments.remove(index);
                                this.schedule_composer_draft_save(cx);
                                cx.notify();
                            }
                            cx.stop_propagation();
                        }
                    })),
            );
            let reveal_path = attachment.path.clone();
            row = row.child(context_menu(
                tile,
                SharedString::from(format!("composer-attachment-{index}-context-menu")),
                &menu,
                move |_| image_preview::attachment_menu_items(reveal_path.clone(), can_reveal),
            ));
        }
        row
    }

    /// The pending follow-up queue between the transcript and the composer: a
    /// single card tucked against the composer's top edge, one row per queued
    /// message. A row pulls its text back into the composer on click and
    /// carries steer/remove/more controls on the right.
    pub(super) fn render_queued_messages(&self, cx: &mut Context<Self>) -> Option<Div> {
        let session_id = self.state.selected_session?;
        let session = self.selected_session()?;
        if session.queued_messages.is_empty() {
            return None;
        }
        let theme = Theme::current(cx);
        let steerable = session.is_busy()
            && session.status != SessionStatus::Connecting
            && self
                .runtimes
                .get(&session.id)
                .is_some_and(|runtime| runtime.driver.supports_steer());
        let mut list = div().flex().flex_col().py(px(4.0));
        for (index, message) in session.queued_messages.iter().enumerate() {
            let message_id = message.id;
            let previous_id = index
                .checked_sub(1)
                .map(|index| session.queued_messages[index].id);
            let next_id = session
                .queued_messages
                .get(index + 1)
                .map(|message| message.id);
            let content = if message.visible_content().trim().is_empty() {
                message
                    .attachments
                    .iter()
                    .map(|attachment| attachment.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                message.visible_content().to_owned()
            };
            let drag = QueuedMessageDrag {
                session_id,
                message_id,
                content: SharedString::from(content),
            };
            let steer_control = steerable.then(|| {
                div()
                    .id(SharedString::from(format!(
                        "queued-message-steer-{message_id}"
                    )))
                    .h(px(28.0))
                    .px(px(8.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_default()
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .hover(|element| element.bg(theme.overlay_strong))
                    .active(|element| element.opacity(0.8))
                    .text_size(ui_px(12.5))
                    .text_color(theme.text_secondary)
                    .child(icon(
                        "icons/corner-down-right.svg",
                        12.0,
                        theme.text_secondary,
                    ))
                    .child(tr!("composer.steer"))
                    .tooltip(Tooltip::text(tr!("composer.steer_current")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.steer_queued_message(session_id, message_id, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.steer_queued_message(session_id, message_id, cx);
                            cx.stop_propagation();
                        }
                    }))
            });
            let menu_handle = self.menu_handle(format!("queued-message-menu-{message_id}"), cx);
            let menu_open = menu_handle.is_open();
            let weak = cx.entity().downgrade();
            let more_control = dropdown_menu(
                div()
                    .id(SharedString::from(format!(
                        "queued-message-more-{message_id}"
                    )))
                    .w(px(26.0))
                    .h(px(26.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .when(menu_open, |element| element.bg(theme.overlay_strong))
                    .hover(|element| element.bg(theme.overlay_strong))
                    .active(|element| element.opacity(0.8))
                    .child(icon("icons/ellipsis.svg", 13.5, theme.text_secondary)),
                SharedString::from(format!("queued-message-more-menu-{message_id}")),
                &menu_handle,
                MenuAlign::BelowRight,
                move |_| {
                    let edit_weak = weak.clone();
                    let remove_weak = weak.clone();
                    vec![
                        MenuItem::new(tr!("composer.edit_in_composer"), move |window, cx| {
                            let _ = edit_weak.update(cx, |this, cx| {
                                this.edit_queued_message(session_id, message_id, window, cx);
                            });
                        })
                        .icon("icons/pencil.svg"),
                        MenuItem::new(tr!("composer.remove_followup"), move |_, cx| {
                            let _ = remove_weak.update(cx, |this, cx| {
                                this.remove_queued_message(session_id, message_id, cx);
                            });
                        })
                        .icon("icons/trash.svg"),
                    ]
                },
            );
            list = list.child(
                div()
                    .id(SharedString::from(format!("queued-message-{message_id}")))
                    .relative()
                    .h(px(32.0))
                    .pl(px(12.0))
                    .pr(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .cursor_default()
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .tooltip(Tooltip::text(tr!("composer.edit_in_composer")))
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "queued-message-drag-{message_id}"
                            )))
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .gap(px(9.0))
                            .cursor_move()
                            .tooltip(Tooltip::text(tr!("composer.reorder_followup")))
                            .child(icon(
                                "icons/chevrons-up-down.svg",
                                13.0,
                                theme.text_tertiary,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_px(13.0))
                                    .text_color(theme.text)
                                    .child(drag.content.clone()),
                            )
                            .on_drag(drag, |drag, cursor_offset, _, cx| {
                                cx.new(|_| QueuedMessageDragPreview {
                                    content: drag.content.clone(),
                                    cursor_offset,
                                })
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .children(steer_control)
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "queued-message-remove-{message_id}"
                                    )))
                                    .w(px(26.0))
                                    .h(px(26.0))
                                    .rounded(px(7.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_default()
                                    .tab_index(0)
                                    .focus_visible(|style| {
                                        style.border_1().border_color(theme.accent)
                                    })
                                    .hover(|element| element.bg(theme.overlay_strong))
                                    .active(|element| element.opacity(0.8))
                                    .child(icon("icons/trash.svg", 13.0, theme.text_secondary))
                                    .tooltip(Tooltip::text(tr!("composer.remove_followup")))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.remove_queued_message(session_id, message_id, cx);
                                    }))
                                    .on_key_down(cx.listener(
                                        move |this, event: &KeyDownEvent, _, cx| {
                                            if matches!(
                                                event.keystroke.key.as_str(),
                                                "enter" | "space"
                                            ) {
                                                this.remove_queued_message(
                                                    session_id, message_id, cx,
                                                );
                                                cx.stop_propagation();
                                            }
                                        },
                                    )),
                            )
                            .child(more_control),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.edit_queued_message(session_id, message_id, window, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.modifiers.alt {
                            let target = match event.keystroke.key.as_str() {
                                "up" => Some((previous_id, false)),
                                "down" => Some((next_id, true)),
                                _ => None,
                            };
                            if let Some((target_id, after)) = target {
                                if let Some(target_id) = target_id {
                                    this.reorder_queued_message(
                                        session_id, message_id, target_id, after, cx,
                                    );
                                }
                                cx.stop_propagation();
                                return;
                            }
                        }
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.edit_queued_message(session_id, message_id, window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    // The two halves provide insertion targets without changing
                    // row layout or intercepting ordinary clicks on its controls.
                    .when(cx.has_active_drag(), |row| {
                        row.children([false, true].map(|after| {
                            div()
                                .id(SharedString::from(format!(
                                    "queued-message-drop-{message_id}-{after}"
                                )))
                                .absolute()
                                .left_0()
                                .right_0()
                                .h(px(16.0))
                                .when(after, |zone| zone.bottom_0())
                                .when(!after, |zone| zone.top_0())
                                // Accept no-op drops too so GPUI always refreshes
                                // after removing the drag preview and drop zones.
                                .drag_over::<QueuedMessageDrag>(move |style, drag, _, _| {
                                    if drag.session_id != session_id
                                        || drag.message_id == message_id
                                    {
                                        return style;
                                    }
                                    let style = if after {
                                        style.border_b_2()
                                    } else {
                                        style.border_t_2()
                                    };
                                    style.border_color(theme.accent)
                                })
                                .on_drop(cx.listener(
                                    move |this, drag: &QueuedMessageDrag, _, cx| {
                                        if drag.session_id == session_id {
                                            this.reorder_queued_message(
                                                session_id,
                                                drag.message_id,
                                                message_id,
                                                after,
                                                cx,
                                            );
                                        }
                                    },
                                ))
                        }))
                    }),
            );
        }
        Some(
            div().flex_none().px(px(20.0)).child(
                div()
                    .w_full()
                    .max_w(px(crate::theme::transcript_width()))
                    .mx_auto()
                    .px(px(14.0))
                    .child(
                        div()
                            .rounded_tl(px(12.0))
                            .rounded_tr(px(12.0))
                            .border_t_1()
                            .border_l_1()
                            .border_r_1()
                            .border_color(theme.border)
                            .bg(theme.composer)
                            // Row hover fills are full-width rectangles; clip
                            // them to the card's rounded corners.
                            .overflow_hidden()
                            .child(list),
                    ),
            ),
        )
    }

    /// What the control row can afford at the width it is being drawn at.
    ///
    /// The row's own content box is arithmetic, not measurement: the chat
    /// column's width is already known from the panel widths, and the card
    /// insets and cap are constants. Only the labels are measured, and only
    /// when they change.
    fn composer_row_plan(
        &self,
        window: &Window,
        traits: Option<&ModelTraits<'_>>,
        paired_send: bool,
    ) -> ComposerRowPlan {
        let session = self.selected_session();
        let interaction = session
            .map(|session| session.interaction_mode)
            .unwrap_or_default();
        let access = session
            .map(|session| session.runtime_mode.access())
            .unwrap_or_default();

        let mut cache = self.composer_row_cache.borrow_mut();
        cache.sync_metrics();
        let traits_label =
            traits.map(|traits| ComposerRowCache::width(&mut cache.traits, &traits.label, window));
        let interaction_label =
            ComposerRowCache::width(&mut cache.interaction, &interaction.label(), window);
        let access_label = ComposerRowCache::width(&mut cache.access, &access.label(), window);

        // The model name is capped at the same width the chip is drawn with,
        // so a name the budget calls affordable is one the row can actually
        // paint. Past the cap it ellipsizes, which costs the reader the tail
        // of the name but keeps it in the row.
        let model_name =
            self.model_display_name(session.and_then(|session| self.model_for_session(session)));
        let model_label = ComposerRowCache::width(&mut cache.model, &model_name, window)
            .min(MODEL_CHIP_MAX_LABEL);
        let send = if paired_send {
            SEND_BUTTON * 2.0 + SEND_PAIR_GAP
        } else {
            SEND_BUTTON
        };
        // Insets the row cannot spend on a chip: the card's border, the
        // row's own horizontal padding, and the send button.
        let fixed = 2.0 * (CARD_BORDER + ROW_INSET_X) + send;
        let widths = ComposerRowWidths {
            model: Some(model_label),
            traits: traits_label,
            interaction: Some(interaction_label),
            access: Some(access_label),
            fixed,
        };
        // The column's width less the gutters around it, capped the same
        // way the card is, so the budget is measured on the same box the
        // row is painted into. The column's own left border is charged to
        // the budget too: it comes out of the same width the chips draw in,
        // and leaving it out would let the row overrun it by a pixel.
        let available =
            (self.chat_viewport_width(window) - 2.0 * CARD_GUTTER_X - CHAT_COLUMN_BORDER)
                .min(crate::theme::transcript_width())
                - fixed;
        composer_row_plan(available, widths)
    }

    pub(super) fn render_composer(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let preparing = session.is_some_and(|session| {
            self.submission_preparations.contains(&session.id)
                || self.response_fork_preparations.contains_key(&session.id)
        });
        let side_question = super::btw::question(self.composer.read(cx).content()).is_some();
        let submit_action = if side_question {
            ComposerSubmitAction::Send
        } else {
            composer_submit_action(session.map(|session| session.status), preparing)
        };
        let escape_stop_armed = session.is_some_and(|session| {
            self.escape_stop_confirmation
                .is_armed_for(EscapeStopTarget::for_session(session), Instant::now())
        });
        let has_draft = !self.composer.read(cx).content().trim().is_empty()
            || !self.composer_attachments.is_empty();
        // Stop stands in for send, and with a draft waiting it is a pair of
        // buttons — the row's budget has to pay for whichever it draws.
        let paired_send = matches!(submit_action, ComposerSubmitAction::Stop) && has_draft;
        let traits = self.model_traits_for_session();
        let plan = self.composer_row_plan(window, traits.as_ref(), paired_send);
        let autocomplete = self.render_composer_autocomplete(window, cx);
        let autocomplete_open = autocomplete.is_some();
        // The chat column accepts the drop and stages attachment chips. The
        // card highlights both under the pointer and while the pointer is
        // anywhere else in that column, so a drag over the transcript reads
        // as the same target. The wash arrives pre-blended because a
        // drag-over refinement replaces the card's fill rather than
        // compositing over it.
        let drop_wash = theme.composer.blend(theme.overlay_strong);
        let drop_ring = theme.accent.opacity(0.7);
        div().flex_none().px(px(20.0)).child(
            div()
                .w_full()
                .max_w(px(crate::theme::transcript_width()))
                .mx_auto()
                .rounded(px(13.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.composer)
                // Horizontal insets live on each row (and inside the field's
                // scroll viewport, via `padding_x`) rather than on the card,
                // so the field's overlay scrollbar can hug the card's edge.
                .py(px(10.0))
                .drag_over::<ExternalPaths>(move |style, _, _, _| {
                    style.bg(drop_wash).border_color(drop_ring)
                })
                .group_drag_over::<ExternalPaths>(CHAT_FILE_DROP_GROUP, move |style| {
                    style.bg(drop_wash).border_color(drop_ring)
                })
                // Anchor for the bounds probe the autocomplete popup aligns to.
                .relative()
                .child(super::autocomplete::composer_card_bounds_probe(
                    self.composer_autocomplete.card_bounds_cell(),
                ))
                // Only while the popup is open: the key context routes the
                // arrows, `enter`, `tab` and `escape` here as actions, out
                // from under the focused field. When it closes, the context
                // disappears with it and `enter` submits again.
                .when(autocomplete_open, |card| {
                    card.key_context("ComposerAutocomplete")
                        .on_action(cx.listener(|this, _: &SelectNextEntry, window, cx| {
                            this.move_autocomplete_highlight("down", window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &SelectPreviousEntry, window, cx| {
                            this.move_autocomplete_highlight("up", window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &ConfirmEntry, window, cx| {
                            this.accept_autocomplete(None, window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &DismissMenu, _, cx| {
                            this.dismiss_autocomplete(cx);
                        }))
                })
                .children(autocomplete)
                .when(!self.composer_attachments.is_empty(), |card| {
                    card.child(self.render_composer_attachments(cx))
                })
                .child(div().pt(px(2.0)).child(self.composer.clone()))
                .child(
                    div()
                        .mt(px(8.0))
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .gap(px(ROW_GAP))
                        .text_size(ui_px(11.5))
                        .line_height(ui_px(14.0))
                        // The budget above has already decided how much
                        // room each chip gets, so the row is drawn at those
                        // widths and only the slack is left to flex. This
                        // is the backstop for a label that measured wider
                        // than it painted: the chips shrink rather than
                        // the row running past the card.
                        .child(self.render_provider_model_control(plan.model_label, cx))
                        .children(self.render_model_traits_control(
                            traits.as_ref(),
                            plan.traits_label,
                            cx,
                        ))
                        .child(self.render_access_control(plan.access_label, cx))
                        .child(self.render_interaction_mode_control(plan.interaction_label, cx))
                        .child(div().flex_1())
                        .child(match submit_action {
                            ComposerSubmitAction::Preparing => div()
                                .id("send-or-stop")
                                .w(px(30.0))
                                .h(px(30.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_default()
                                .bg(theme.overlay_strong)
                                .child(motion::spin(icon(
                                    "icons/loader-circle.svg",
                                    17.0,
                                    theme.text_secondary,
                                )))
                                .tooltip(Tooltip::text(tr!("composer.preparing_task"))),
                            ComposerSubmitAction::Stop => div()
                                .id("working-actions")
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .id("send-or-stop")
                                        .w(px(30.0))
                                        .h(px(30.0))
                                        .rounded_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_default()
                                        .bg(theme.overlay_strong)
                                        .hover(|element| element.bg(theme.danger_soft))
                                        .active(|element| element.opacity(0.8))
                                        .when(escape_stop_armed, |element| {
                                            element.child(
                                                div()
                                                    .text_size(ui_px(10.5))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.text)
                                                    .child("Esc"),
                                            )
                                        })
                                        .when(!escape_stop_armed, |element| {
                                            element.child(icon("icons/stop.svg", 20.0, theme.text))
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cancel_turn(cx);
                                        })),
                                )
                                .when(has_draft, |element| {
                                    element.child(
                                        div()
                                            .id("queue-follow-up")
                                            .w(px(30.0))
                                            .h(px(30.0))
                                            .rounded_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_default()
                                            .bg(theme.inverse)
                                            .hover(|element| element.opacity(0.9))
                                            .active(|element| element.opacity(0.8))
                                            .child(icon(
                                                "icons/arrow-up.svg",
                                                18.0,
                                                theme.on_inverse,
                                            ))
                                            .tooltip(Tooltip::text(tr!("composer.queue_followup")))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                let prompt =
                                                    this.composer.read(cx).content().to_owned();
                                                if let Some(submission) =
                                                    this.submission_with_attachments(&prompt, cx)
                                                {
                                                    this.composer
                                                        .update(cx, |input, cx| input.clear(cx));
                                                    this.submit_composer_submission(submission, cx);
                                                }
                                            })),
                                    )
                                }),
                            ComposerSubmitAction::Send => div()
                                .id("send-or-stop")
                                .w(px(30.0))
                                .h(px(30.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if has_draft {
                                    theme.inverse
                                } else {
                                    theme.overlay_strong
                                })
                                .when(has_draft, |element| {
                                    element
                                        .cursor_default()
                                        .hover(|element| element.opacity(0.9))
                                        .active(|element| element.opacity(0.8))
                                })
                                .child(icon(
                                    "icons/arrow-up.svg",
                                    18.0,
                                    if has_draft {
                                        theme.on_inverse
                                    } else {
                                        theme.text_ghost
                                    },
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let prompt = this.composer.read(cx).content().to_owned();
                                    if let Some(submission) =
                                        this.submission_with_attachments(&prompt, cx)
                                    {
                                        this.composer.update(cx, |input, cx| input.clear(cx));
                                        this.submit_composer_submission(submission, cx);
                                    }
                                })),
                        }),
                ),
        )
    }

    fn render_branch_selector(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = Theme::current(cx);
        let session = self.selected_session()?;
        let workspace = session.workspace.clone();
        let workspace_path = self.workspace_path_for_session(session)?.to_path_buf();
        self.selected_project()
            .filter(|project| !project.is_projectless())?;
        let branch_enabled = !session.is_busy() && !self.branch_operation_pending;
        let planned_worktree = matches!(workspace, SessionWorkspace::NewWorktree { .. });
        let snapshot = self.branch_snapshot_for_workspace(&workspace_path, cx)?;
        let selected_branch = match &workspace {
            SessionWorkspace::Local => snapshot.display_branch().map(str::to_owned),
            SessionWorkspace::NewWorktree { base_branch } => base_branch
                .clone()
                .or_else(|| snapshot.default_branch.clone())
                .or_else(|| snapshot.display_branch().map(str::to_owned)),
            SessionWorkspace::Worktree { branch, .. } => snapshot
                .current
                .clone()
                .or_else(|| Some(branch.clone()))
                .or_else(|| snapshot.detached_head.clone()),
        }
        .unwrap_or_else(|| tr!("branches.detached_head"));

        let weak = cx.entity().downgrade();
        let search = self.branch_search.clone();
        let create_input = self.branch_create_input.clone();
        let search_focus = search.read(cx).focus_handle(cx);
        let handle = {
            let toggle_weak = weak.clone();
            let reset_search = search.clone();
            let reset_create = create_input.clone();
            let picker_focus = search_focus.clone();
            self.menu_handle_with(BRANCH_PICKER_MENU_ID, cx, move |open, window, cx| {
                let _ = toggle_weak.update(cx, |this, cx| {
                    if open {
                        this.branch_picker_mode = BranchPickerMode::Browse;
                        this.branch_picker_highlight = None;
                        let project_name = this
                            .selected_project()
                            .map(Project::display_name)
                            .unwrap_or_else(|| tr!("project.project_lower"));
                        reset_search.update(cx, |input, cx| {
                            input.set_placeholder(
                                tr!("branches.search_project", project = project_name),
                                cx,
                            );
                            input.clear(cx);
                        });
                        reset_create.update(cx, |input, cx| input.clear(cx));
                        this.refresh_selected_branch_snapshot(cx);
                    } else {
                        this.branch_picker_mode = BranchPickerMode::Browse;
                        let focus = this.composer_focus(cx);
                        window.focus(&focus, cx);
                    }
                    cx.notify();
                });
                if open {
                    let picker_focus = picker_focus.clone();
                    window.on_next_frame(move |window, _| {
                        window.on_next_frame(move |window, cx| window.focus(&picker_focus, cx));
                    });
                }
            })
        };

        let trigger = MenuChip::new("workspace-branch")
            .icon("icons/git-branch.svg", theme.text_tertiary)
            .label(if self.branch_operation_pending {
                tr!("branches.switching")
            } else {
                selected_branch.clone()
            })
            .caret(false)
            .disabled(!branch_enabled)
            .selected(branch_enabled && handle.is_open())
            .max_w(px(210.0));
        if !branch_enabled {
            return Some(trigger.into_any_element());
        }

        let normalized_query = self
            .branch_search
            .read(cx)
            .content()
            .trim()
            .to_ascii_lowercase();
        let visible_branches = Rc::new(
            if handle.is_open() && self.branch_picker_mode == BranchPickerMode::Browse {
                visible_branch_entries(&snapshot.branches, &selected_branch, &normalized_query)
            } else {
                Vec::new()
            },
        );
        let allow_create = !planned_worktree;
        let actions = Rc::new(
            visible_branches
                .iter()
                .filter(|branch| planned_worktree || !branch.checked_out_elsewhere)
                .map(|branch| BranchPickerAction::Checkout(branch.name.clone()))
                .chain(allow_create.then_some(BranchPickerAction::Create))
                .collect::<Vec<_>>(),
        );
        let highlight = self
            .branch_picker_highlight
            .filter(|index| *index < actions.len());
        let mode = self.branch_picker_mode;
        if handle.is_open() && mode == BranchPickerMode::Browse {
            self.sync_branch_picker_rows(&visible_branches);
        }
        let branch_list = self.branch_picker_list_state.clone();

        Some(popover(
            trigger,
            &handle,
            MenuAlign::AboveLeft,
            move |popover, _window, _cx| {
                let popover = popover.clone();
                let next_actions = actions.clone();
                let previous_actions = actions.clone();
                let confirm_actions = actions.clone();
                let next_weak = weak.clone();
                let previous_weak = weak.clone();
                let confirm_weak = weak.clone();
                let confirm_popover = popover.clone();

                let body = if mode == BranchPickerMode::Create {
                    div()
                        .w_full()
                        .p(px(14.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .text_size(ui_px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(icon("icons/plus.svg", 14.0, theme.text_secondary))
                                .child(tr!("branches.create_and_checkout")),
                        )
                        .child(
                            div()
                                .mt(px(12.0))
                                .h(px(36.0))
                                .px(px(10.0))
                                .rounded(px(9.0))
                                .border_1()
                                .border_color(theme.border_strong)
                                .bg(theme.surface)
                                .flex()
                                .items_center()
                                .child(div().flex_1().min_w_0().child(create_input.clone())),
                        )
                        .child(
                            div()
                                .mt(px(9.0))
                                .text_size(ui_px(10.5))
                                .text_color(theme.text_tertiary)
                                .child(tr!("branches.create_hint")),
                        )
                        .into_any_element()
                } else {
                    let rows = if visible_branches.is_empty() {
                        div()
                            .id("branch-picker-list-empty")
                            .h(px(64.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(ui_px(11.5))
                            .text_color(theme.text_muted)
                            .child(tr!("branches.none_found"))
                            .into_any_element()
                    } else {
                        let list_branches = visible_branches.clone();
                        let list_actions = actions.clone();
                        let list_selected_branch = selected_branch.clone();
                        let list_weak = weak.clone();
                        let list_popover = popover.clone();
                        let height =
                            (visible_branches.len() as f32 * BRANCH_PICKER_ROW_HEIGHT).min(260.0);
                        div()
                            .id("branch-picker-list")
                            .w_full()
                            .h(px(height))
                            .flex_none()
                            .px(px(4.0))
                            .child(
                                list(branch_list.clone(), move |index, _window, _cx| {
                                    let Some(branch) = list_branches.get(index) else {
                                        return div().into_any_element();
                                    };
                                    let selected = branch.name == list_selected_branch;
                                    let disabled =
                                        branch.checked_out_elsewhere && !planned_worktree;
                                    let highlighted = highlight
                                        .and_then(|index| list_actions.get(index))
                                        .is_some_and(|action| {
                                            matches!(
                                                action,
                                                BranchPickerAction::Checkout(name)
                                                    if name == &branch.name
                                            )
                                        });
                                    let color = if disabled {
                                        theme.text_ghost
                                    } else {
                                        theme.text
                                    };
                                    let row = div()
                                        .id(SharedString::from(format!(
                                            "branch-row-{}",
                                            branch.name
                                        )))
                                        .w_full()
                                        .h(px(BRANCH_PICKER_ROW_HEIGHT))
                                        .px(px(8.0))
                                        .rounded(px(6.0))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .cursor_default()
                                        .when(highlighted, |element| {
                                            element.bg(theme.overlay_strong)
                                        })
                                        .when(!disabled, |element| {
                                            element
                                                .hover(|element| element.bg(theme.overlay))
                                                .active(|element| element.opacity(0.85))
                                        })
                                        .child(icon("icons/git-branch.svg", 13.0, color))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .flex_1()
                                                .truncate()
                                                .text_size(ui_px(12.5))
                                                .line_height(ui_px(16.0))
                                                .text_color(color)
                                                .child(SharedString::from(branch.name.clone())),
                                        )
                                        .when(selected, |element| {
                                            element.child(icon(
                                                "icons/check.svg",
                                                12.0,
                                                theme.text_secondary,
                                            ))
                                        });
                                    if disabled {
                                        row.into_any_element()
                                    } else {
                                        let branch_name = branch.name.clone();
                                        let select_weak = list_weak.clone();
                                        let select_popover = list_popover.clone();
                                        row.on_click(move |_, window, cx| {
                                            let should_close = select_weak
                                                .update(cx, |this, cx| {
                                                    this.choose_workspace_branch(
                                                        branch_name.clone(),
                                                        cx,
                                                    )
                                                })
                                                .unwrap_or(false);
                                            if should_close {
                                                select_popover.close(window, cx);
                                                window.refresh();
                                            }
                                        })
                                        .into_any_element()
                                    }
                                })
                                .size_full(),
                            )
                            .into_any_element()
                    };

                    let create_row = allow_create.then(|| {
                        let create_weak = weak.clone();
                        div()
                            .id("create-workspace-branch")
                            .mx(px(4.0))
                            .h(px(BRANCH_PICKER_ROW_HEIGHT))
                            .px(px(8.0))
                            .rounded(px(6.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .cursor_default()
                            .when(
                                highlight.and_then(|index| actions.get(index))
                                    == Some(&BranchPickerAction::Create),
                                |element| element.bg(theme.overlay_strong),
                            )
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.opacity(0.85))
                            .child(icon("icons/plus.svg", 13.0, theme.text_secondary))
                            .child(
                                div()
                                    .text_size(ui_px(12.5))
                                    .line_height(ui_px(16.0))
                                    .text_color(theme.text)
                                    .child(tr!("branches.create_and_checkout_ellipsis")),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = create_weak.update(cx, |this, cx| {
                                    this.begin_branch_creation(window, cx);
                                });
                            })
                    });

                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(54.0))
                                .px(px(12.0))
                                .pt(px(10.0))
                                .pb(px(8.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(36.0))
                                        .px(px(10.0))
                                        .rounded(px(9.0))
                                        .bg(theme.surface)
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .child(icon("icons/search.svg", 15.0, theme.text_secondary))
                                        .child(div().flex_1().min_w_0().child(search.clone())),
                                ),
                        )
                        .child(
                            div()
                                .px(px(14.0))
                                .pt(px(3.0))
                                .pb(px(7.0))
                                .text_size(ui_px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text_tertiary)
                                .child(tr!("branches.title")),
                        )
                        .child(rows)
                        .when_some(create_row, |element, create_row| {
                            element
                                .child(div().mx(px(6.0)).my(px(4.0)).h(px(1.0)).bg(theme.border))
                                .child(create_row)
                                .child(div().h(px(4.0)))
                        })
                        .into_any_element()
                };

                div()
                    .w(px(360.0))
                    .max_h(px(390.0))
                    .rounded(px(13.0))
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .on_action(move |_: &SelectNextEntry, _, cx| {
                        let _ = next_weak.update(cx, |this, cx| {
                            this.move_branch_picker_highlight("down", &next_actions, cx);
                        });
                    })
                    .on_action(move |_: &SelectPreviousEntry, _, cx| {
                        let _ = previous_weak.update(cx, |this, cx| {
                            this.move_branch_picker_highlight("up", &previous_actions, cx);
                        });
                    })
                    .on_action(move |_: &ConfirmEntry, window, cx| {
                        let should_close = confirm_weak
                            .update(cx, |this, cx| {
                                this.confirm_branch_picker_action(&confirm_actions, window, cx)
                            })
                            .unwrap_or(false);
                        if should_close {
                            confirm_popover.close(window, cx);
                            window.refresh();
                        }
                    })
                    .child(body)
                    .into_any_element()
            },
        ))
    }

    pub(super) fn render_workspace_footer(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let selected_project_id = self.state.selected_project;
        let projectless_selected = self.selected_project().is_some_and(Project::is_projectless);
        let project_name = self
            .selected_project()
            .map(|project| {
                if project.is_projectless() {
                    tr!("project.choose_project")
                } else {
                    project.display_name()
                }
            })
            .unwrap_or_else(|| tr!("project.choose_project"));
        let can_configure_workspace = self
            .selected_session()
            .is_some_and(|session| !session.has_started() && !session.is_busy());

        let project_handle = self.menu_handle("workspace-project", cx);
        let project_trigger = MenuChip::new("workspace-project")
            .icon("icons/folder.svg", theme.text_tertiary)
            .label(project_name)
            .caret(false)
            .disabled(!can_configure_workspace)
            .selected(can_configure_workspace && project_handle.is_open())
            .max_w(px(190.0));
        let project_selector = if can_configure_workspace {
            let project_options = self
                .state
                .projects
                .iter()
                .filter(|project| !project.is_projectless())
                .filter(|project| Some(project.id) == selected_project_id)
                .chain(
                    self.state
                        .projects
                        .iter()
                        .filter(|project| !project.is_projectless())
                        .filter(|project| Some(project.id) != selected_project_id),
                )
                .map(|project| (project.id, project.display_name()))
                .collect::<Vec<_>>();
            let weak = cx.entity().downgrade();
            dropdown_menu(
                project_trigger,
                "workspace-project-menu",
                &project_handle,
                MenuAlign::AboveLeft,
                move |_| {
                    let mut items = project_options
                        .clone()
                        .into_iter()
                        .map(|(project_id, project_name)| {
                            let weak = weak.clone();
                            MenuItem::new(project_name, move |_, cx| {
                                if Some(project_id) != selected_project_id {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.select_project_from_composer(project_id, cx);
                                    });
                                }
                            })
                            .selected(Some(project_id) == selected_project_id)
                        })
                        .collect::<Vec<_>>();
                    if !items.is_empty() {
                        items.push(MenuItem::Separator);
                    }
                    let add_project = weak.clone();
                    items.push(
                        MenuItem::new(tr!("project.new_project"), move |_, cx| {
                            let _ = add_project.update(cx, |this, cx| this.add_project(cx));
                        })
                        .icon("icons/folder-new.svg"),
                    );
                    let projectless = weak.clone();
                    items.push(
                        MenuItem::new(tr!("project.no_project"), move |_, cx| {
                            let _ = projectless.update(cx, |this, cx| {
                                if !this.selected_project().is_some_and(Project::is_projectless) {
                                    this.create_projectless_session_from_composer(cx);
                                }
                            });
                        })
                        .icon("icons/x.svg")
                        .selected(projectless_selected),
                    );
                    items
                },
            )
        } else {
            project_trigger.into_any_element()
        };

        let workspace = self
            .selected_session()
            .map(|session| session.workspace.clone())
            .unwrap_or_default();
        let workspace_label = match &workspace {
            SessionWorkspace::Local => SharedString::from(tr!("workspace.local")),
            SessionWorkspace::NewWorktree { .. } => {
                SharedString::from(tr!("workspace.new_worktree"))
            }
            SessionWorkspace::Worktree { branch, .. } => SharedString::from(branch.clone()),
        };
        let workspace_icon = if workspace.is_local() {
            "icons/laptop.svg"
        } else {
            "icons/fork.svg"
        };
        let worktree_handle = self.menu_handle("workspace-worktree", cx);
        let worktree_trigger = MenuChip::new("workspace-worktree")
            .icon(workspace_icon, theme.text_tertiary)
            .label(workspace_label)
            .caret(false)
            .disabled(!can_configure_workspace)
            .selected(can_configure_workspace && worktree_handle.is_open())
            .max_w(px(180.0));
        let worktree_selector = if can_configure_workspace {
            let local_selected = workspace.is_local();
            let worktree_selected = workspace.is_worktree();
            let weak = cx.entity().downgrade();
            dropdown_menu(
                worktree_trigger,
                "workspace-worktree-menu",
                &worktree_handle,
                MenuAlign::AboveLeft,
                move |_| {
                    let local = weak.clone();
                    let worktree = weak.clone();
                    vec![
                        MenuItem::Header(tr!("workspace.work_in").into()),
                        MenuItem::new(tr!("workspace.local"), move |_, cx| {
                            let _ = local.update(cx, |this, cx| {
                                this.select_workspace(SessionWorkspace::Local, cx);
                            });
                        })
                        .icon("icons/laptop.svg")
                        .selected(local_selected),
                        MenuItem::new(tr!("workspace.new_worktree"), move |_, cx| {
                            let _ = worktree.update(cx, |this, cx| {
                                this.select_workspace(
                                    SessionWorkspace::NewWorktree { base_branch: None },
                                    cx,
                                );
                            });
                        })
                        .icon("icons/fork.svg")
                        .selected(worktree_selected)
                        .disabled(projectless_selected),
                    ]
                },
            )
        } else {
            worktree_trigger.into_any_element()
        };

        let branch_selector = self.render_branch_selector(cx);

        let usage_meter = self.render_usage_meter(cx);
        div()
            .flex_none()
            .px(px(20.0))
            .pb(px(8.0))
            .pt(px(4.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(crate::theme::transcript_width()))
                    .mx_auto()
                    .h(px(32.0))
                    // The chip contributes 9px, lining its icon up with the
                    // composer's 10px padding plus the controls' 9px inset.
                    .pl(px(10.0))
                    .pr(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .tab_index(0)
                    .tab_group()
                    .tab_stop(false)
                    .text_size(ui_px(11.5))
                    .line_height(ui_px(15.0))
                    .child(project_selector)
                    .child(worktree_selector)
                    .children(branch_selector)
                    .child(div().flex_1())
                    .children(usage_meter),
            )
    }
}

/// Branches matching the search, with the selected branch pinned first and
/// every other row sorted by name. Disabled worktree-owned rows stay in the
/// result; the UI needs to explain why Git cannot switch to them.
pub(super) fn visible_branch_entries(
    branches: &[crate::git_branch::BranchEntry],
    selected_branch: &str,
    normalized_query: &str,
) -> Vec<crate::git_branch::BranchEntry> {
    let normalized_query = normalized_query.to_ascii_lowercase();
    let mut visible = branches
        .iter()
        .filter(|branch| {
            normalized_query
                .split_whitespace()
                .all(|token| branch.name.to_ascii_lowercase().contains(token))
        })
        .cloned()
        .collect::<Vec<_>>();
    visible.sort_by(|left, right| {
        let left_selected = left.name == selected_branch;
        let right_selected = right.name == selected_branch;
        right_selected
            .cmp(&left_selected)
            .then_with(|| left.name.cmp(&right.name))
    });
    visible
}

/// The mention a dropped file submits: relative to the project root when the
/// file is inside it, absolute otherwise, directories with a trailing slash —
/// the same form the `@` autocomplete inserts. Dropping the root itself keeps
/// the absolute path rather than producing an empty mention.
// Base64 keeps the authenticated JSON transport browser-compatible but adds
// one third of wire overhead. Stay comfortably below tungstenite's default
// message limit until uploads move to a streaming content endpoint.
const MAX_ATTACHMENT_BYTES: u64 = fintwind_client::attachments::MAX_ATTACHMENT_BYTES as u64;
const MAX_PROMPT_FILE_BYTES: u64 = fintwind_client::attachments::MAX_PROMPT_FILE_BYTES as u64;

/// Reads a client-local drop into an upload payload. This is the explicit
/// client/daemon boundary: none of these source paths are persisted or handed
/// to a provider.
fn attachment_upload_from_path(
    source: &Path,
) -> anyhow::Result<(
    String,
    fintwind_client::attachments::AttachmentUpload,
    Option<Vec<u8>>,
)> {
    let metadata = std::fs::symlink_metadata(source)
        .with_context(|| format!("could not read attachment {}", source.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!(
            "symbolic-link attachments are not supported: {}",
            source.display()
        );
    }
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("attachment has no file name: {}", source.display()))?
        .to_owned();
    if metadata.is_file() {
        if metadata.len() > MAX_PROMPT_FILE_BYTES {
            anyhow::bail!("attachment is larger than 20 MB: {}", source.display());
        }
        let bytes = std::fs::read(source)
            .with_context(|| format!("could not read attachment {}", source.display()))?;
        let is_image = is_image_attachment_path(source);
        return Ok((
            name,
            fintwind_client::attachments::AttachmentUpload::File {
                data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            },
            is_image.then_some(bytes),
        ));
    }
    if !metadata.is_dir() {
        anyhow::bail!(
            "attachment is not a file or directory: {}",
            source.display()
        );
    }

    let mut pending = vec![source.to_path_buf()];
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).with_context(|| {
            format!(
                "could not read attachment directory {}",
                directory.display()
            )
        })? {
            let entry = entry?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if entries.len() >= fintwind_client::attachments::MAX_ATTACHMENT_FILES {
                anyhow::bail!(
                    "attachment directory contains more than {} files",
                    fintwind_client::attachments::MAX_ATTACHMENT_FILES
                );
            }
            total_bytes = total_bytes.saturating_add(metadata.len());
            if total_bytes > MAX_ATTACHMENT_BYTES {
                anyhow::bail!("attachment directory is larger than 32 MB");
            }
            let relative_path = path
                .strip_prefix(source)
                .context("attachment entry escaped its source directory")?
                .to_path_buf();
            let bytes = std::fs::read(&path)
                .with_context(|| format!("could not read attachment {}", path.display()))?;
            entries.push(fintwind_client::attachments::AttachmentUploadEntry {
                relative_path,
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            });
        }
    }
    Ok((
        name,
        fintwind_client::attachments::AttachmentUpload::Directory { entries },
        None,
    ))
}

#[cfg(test)]
pub(super) fn dropped_file_mention(
    root: Option<&std::path::Path>,
    path: &std::path::Path,
    is_dir: bool,
) -> String {
    let mention = root
        .and_then(|root| path.strip_prefix(root).ok())
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or(path)
        .display()
        .to_string();
    if is_dir && !mention.ends_with('/') {
        format!("{mention}/")
    } else {
        mention
    }
}

fn is_image_attachment_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "webp"
                    | "bmp"
                    | "svg"
                    | "tif"
                    | "tiff"
                    | "ico"
                    | "pnm"
                    | "pbm"
                    | "pgm"
                    | "ppm"
            )
        })
}

/// The text a submission sends. Attachments are not appended; an empty prompt
/// is still sent when chips are staged, and `None` means there is nothing.
pub(super) fn submission_text(prompt: &str, attachment_count: usize) -> Option<String> {
    let prompt = prompt.trim();
    if prompt.is_empty() && attachment_count == 0 {
        None
    } else {
        Some(prompt.to_owned())
    }
}

/// Where the picker's keyboard cursor lands, wrapping at both ends.
///
/// `None` for `current` means the cursor has not moved yet, so `down` opens on
/// the first row and `up` on the last. `None` in the result means the key does
/// not navigate.
pub(super) fn next_picker_highlight(
    current: Option<usize>,
    len: usize,
    key: &str,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match key {
        "down" => Some(current.map_or(0, |index| (index + 1) % len)),
        "up" => Some(current.map_or(len - 1, |index| (index + len - 1) % len)),
        _ => None,
    }
}

/// The picker sidebar tabs, in stable rail order: favorites first, then one
/// entry per installed model provider in first-appearance order.
pub(super) fn visible_picker_tabs(probes: &[ProviderProbe]) -> Vec<ModelPickerTab> {
    let mut tabs = vec![ModelPickerTab::Favorites];
    for model in probes
        .iter()
        .filter(|probe| probe.installed)
        .flat_map(|probe| &probe.models)
    {
        let provider = model_picker_provider_label(model.sub_provider.as_deref());
        let tab = ModelPickerTab::Provider(provider);
        if !tabs.contains(&tab) {
            tabs.push(tab);
        }
    }
    tabs
}

pub(super) fn model_picker_provider_label(sub_provider: Option<&str>) -> String {
    match sub_provider.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) if name.eq_ignore_ascii_case("opencode") => "OpenCode".to_owned(),
        Some(name) => name.to_owned(),
        None => "OpenCode".to_owned(),
    }
}

/// The models the picker lists, in display order.
///
/// Shared by the panel body and by `enter`'s handler so a keyboard cursor index
/// always means the same row in both.
pub(super) fn visible_picker_models(
    probes: &[ProviderProbe],
    favorites: &[FavoriteModel],
    selected_tab: ModelPickerTab,
    normalized_query: &str,
) -> Vec<ProviderModel> {
    let searching = !normalized_query.is_empty();
    let mut models: Vec<ProviderModel> = probes
        .iter()
        .filter(|probe| probe.installed)
        .flat_map(|probe| probe.models.iter().cloned())
        .filter(|model| {
            if searching {
                let searchable = format!(
                    "{} {} {}",
                    model.name,
                    model.id,
                    model.sub_provider.as_deref().unwrap_or("")
                )
                .to_ascii_lowercase();
                return normalized_query
                    .split_whitespace()
                    .all(|token| searchable.contains(token));
            }
            match &selected_tab {
                ModelPickerTab::Favorites => {
                    favorites.iter().any(|favorite| favorite.model == model.id)
                }
                ModelPickerTab::Provider(provider) => {
                    model_picker_provider_label(model.sub_provider.as_deref()) == *provider
                }
            }
        })
        .collect();
    if !searching && selected_tab == ModelPickerTab::Favorites {
        models.sort_by_key(|model| {
            favorites
                .iter()
                .position(|favorite| favorite.model == model.id)
                .unwrap_or(usize::MAX)
        });
    }
    models
}
