//! Source toggle captures its owning view during rendering, not input dispatch.

use super::*;
use crate::ui::scrollbar::{self, Scrollable, ScrollbarState};
use gpui::{FocusHandle, FollowMode, ListAlignment, ListOffset, ListState, list};

pub(super) fn theme(palette: &Palette) -> fintwind_diagram::diagram_compile::Theme {
    let rgb = |color: Hsla| {
        let color: gpui::Rgba = color.into();
        [color.r, color.g, color.b].map(|value| (value * 255.0).round() as u8)
    };
    fintwind_diagram::diagram_compile::Theme {
        background: rgb(palette.inset),
        foreground: rgb(palette.text),
        muted: rgb(palette.tertiary),
        border: rgb(palette.border),
        accent: rgb(palette.accent),
    }
}

#[derive(IntoElement)]
pub(super) struct SourceToggle {
    pub key: TextKey,
    pub visible: Rc<RefCell<HashSet<usize>>>,
    pub show_source: bool,
    pub palette: Palette,
    pub source: std::sync::Arc<super::super::diagram::Source>,
    pub theme: fintwind_diagram::diagram_compile::Theme,
    pub font_scale: f32,
}

impl gpui::RenderOnce for SourceToggle {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        if !self.show_source
            && !super::super::diagram::is_ready(
                self.source,
                self.theme,
                self.font_scale,
                window,
                cx,
            )
        {
            return div().into_any_element();
        }
        let owner = window.current_view();
        let ordinal = self.key.index;
        let visible = self.visible;
        let keyboard_visible = visible.clone();
        div()
            .id(SharedString::from(format!(
                "diagram-source-{}-{}",
                self.key.row, ordinal
            )))
            .tab_index(0)
            .size(px(24.0))
            .flex_none()
            .rounded(px(5.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(self.palette.accent))
            .hover(|style| style.bg(self.palette.overlay))
            .active(|style| style.bg(self.palette.overlay))
            .tooltip(Tooltip::text(if self.show_source {
                tr!("common.show_diagram")
            } else {
                tr!("common.show_source")
            }))
            .child(crate::ui::icon(
                if self.show_source {
                    "icons/eye.svg"
                } else {
                    "icons/file-types/console.svg"
                },
                12.0,
                self.palette.ghost,
            ))
            .on_click(move |_, _, cx| {
                let mut visible = visible.borrow_mut();
                if !visible.remove(&ordinal) {
                    visible.insert(ordinal);
                }
                cx.notify(owner);
                cx.stop_propagation();
            })
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    let mut visible = keyboard_visible.borrow_mut();
                    if !visible.remove(&ordinal) {
                        visible.insert(ordinal);
                    }
                    cx.notify(owner);
                    cx.stop_propagation();
                }
            })
            .into_any_element()
    }
}

#[derive(IntoElement)]
pub(super) struct ExpandDiagram {
    pub key: TextKey,
    pub palette: Palette,
    pub source: std::sync::Arc<super::super::diagram::Source>,
    pub theme: fintwind_diagram::diagram_compile::Theme,
    pub font_scale: f32,
}

impl gpui::RenderOnce for ExpandDiagram {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let Some(preview) = super::super::diagram::preview_image(
            self.source,
            self.theme,
            self.font_scale,
            window,
            cx,
        ) else {
            return div().into_any_element();
        };
        let owner = window.current_view();
        let ordinal = self.key.index;
        let keyboard_preview = preview.clone();
        let keyboard_owner = owner;
        div()
            .id(SharedString::from(format!(
                "diagram-expand-{}-{ordinal}",
                self.key.row
            )))
            .tab_index(0)
            .size(px(24.0))
            .flex_none()
            .rounded(px(5.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(self.palette.accent))
            .hover(|style| style.bg(self.palette.overlay))
            .active(|style| style.bg(self.palette.overlay))
            .tooltip(Tooltip::text(tr!("diagram.expand_diagram")))
            .child(crate::ui::icon(
                "icons/window-maximize.svg",
                12.0,
                self.palette.ghost,
            ))
            .on_click(move |_, _, cx| {
                super::super::diagram::request_preview(preview.clone(), cx);
                cx.notify(owner);
                cx.stop_propagation();
            })
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    super::super::diagram::request_preview(keyboard_preview.clone(), cx);
                    cx.notify(keyboard_owner);
                    cx.stop_propagation();
                }
            })
            .into_any_element()
    }
}

/// Source fallback is a bounded native list, never one 32 KiB text element.
pub(super) struct SourceDocument {
    parts: Rc<Vec<Rc<FlatText>>>,
    state: Rc<SourceState>,
}

/// Scroll and focus survive streamed tail replacement and theme changes.
pub(super) struct SourceState {
    list: ListState,
    focus: RefCell<Option<FocusHandle>>,
    scrollbar: Rc<ScrollbarState>,
    rows: Cell<usize>,
    streaming: Cell<bool>,
}

impl SourceState {
    pub(super) fn new() -> Self {
        Self {
            list: ListState::new(0, ListAlignment::Top, px(64.0)),
            focus: RefCell::new(None),
            scrollbar: ScrollbarState::new(),
            rows: Cell::new(0),
            streaming: Cell::new(false),
        }
    }
}

impl SourceDocument {
    pub(super) fn len(&self) -> usize {
        self.parts.len()
    }

    pub(super) fn new(code: &str, palette: &Palette, state: Rc<SourceState>) -> Self {
        let mut code_font = font(mono_family());
        code_font.weight = FontWeight::NORMAL;
        let parts = super::super::virtualized::text_parts(code)
            .into_iter()
            .map(|part| {
                Rc::new(FlatText {
                    text: SharedString::from(part.to_owned()),
                    runs: code_runs(part, None, &code_font, palette),
                    links: Vec::new(),
                    active_link: Rc::default(),
                    code_ranges: Vec::new(),
                    math: None,
                    references: Vec::new(),
                    allow_references: false,
                })
            })
            .collect::<Vec<_>>();
        let old = state.rows.replace(parts.len());
        let count = parts.len();
        if old != count {
            state
                .list
                .splice(old.min(count)..old, count.saturating_sub(old));
        }
        // Content/style updates only, never per-frame: every text row is small.
        state.list.remeasure_items(0..count);
        Self {
            parts: Rc::new(parts),
            state,
        }
    }
}

#[derive(IntoElement)]
pub(super) struct SourceText {
    pub document: Rc<SourceDocument>,
    pub key: TextKey,
    pub selection: TranscriptSelection,
    pub palette: Palette,
    pub metrics: Metrics,
    pub streaming: bool,
}

impl gpui::RenderOnce for SourceText {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let owner = window.current_view();
        let focus = self
            .document
            .state
            .focus
            .borrow_mut()
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        let state = self.document.state.list.clone();
        if self.document.state.streaming.replace(self.streaming) != self.streaming {
            state.set_follow_mode(if self.streaming {
                FollowMode::Tail
            } else {
                FollowMode::Normal
            });
        }
        state.set_scroll_handler(move |_, _, cx| cx.notify(owner));
        let keyboard = state.clone();
        let wheel = state.clone();
        let parts = self.document.parts.clone();
        let palette = self.palette;
        let metrics = self.metrics;
        let row_id = self.key.row.clone();
        let ordinal = self.key.index;
        let selection = self.selection;
        div()
            .id(SharedString::from(format!(
                "diagram-text-{}-{ordinal}",
                self.key.row
            )))
            .track_focus(&focus)
            .tab_index(0)
            .w_full()
            .min_w_0()
            .h(px((metrics.code_line_height * 14.0).clamp(140.0, 480.0)))
            .relative()
            .overflow_hidden()
            .focus_visible(|style| style.border_1().border_color(palette.accent))
            .child(
                list(state.clone(), move |index, _, _| {
                    let ctx = Ctx::new(row_id.clone(), &palette, metrics, selection.clone())
                        .with_streaming_animation(false);
                    ctx.starts_block.set(index == 0);
                    div()
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .text_size(px(metrics.code_text_size))
                        .line_height(px(metrics.code_line_height))
                        .text_color(palette.secondary)
                        .child(text_element(
                            &parts[index],
                            TextKey::new(row_id.clone(), ordinal + 1 + index),
                            &ctx,
                        ))
                        .into_any_element()
                })
                .size_full(),
            )
            .child(scrollbar::vertical(&state, &self.document.state.scrollbar))
            .on_scroll_wheel(move |_, _, cx| {
                if wheel.max_offset() > px(0.5) {
                    cx.stop_propagation();
                }
            })
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let delta = match event.keystroke.key.as_str() {
                    "up" => -24.0,
                    "down" => 24.0,
                    "pageup" => -240.0,
                    "pagedown" => 240.0,
                    "home" => {
                        keyboard.pause_following_tail();
                        keyboard.scroll_to(ListOffset::default());
                        0.0
                    }
                    "end" => {
                        if self.streaming {
                            keyboard.set_follow_mode(FollowMode::Normal);
                            keyboard.set_follow_mode(FollowMode::Tail);
                        } else {
                            Scrollable::scroll_to(&keyboard, keyboard.max_offset());
                        }
                        0.0
                    }
                    _ => return,
                };
                if delta != 0.0 {
                    keyboard.pause_following_tail();
                    Scrollable::scroll_to(
                        &keyboard,
                        (keyboard.scrolled() + px(delta)).max(px(0.0)),
                    );
                }
                cx.notify(owner);
                cx.stop_propagation();
            })
    }
}
