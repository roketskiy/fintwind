//! Window-modal preview for Mermaid diagrams (Issue #13).
//!
//! Inline diagrams shrink to the transcript column (`max_w_full`), so a wide
//! flowchart becomes unreadable. This overlay reuses the cached raster — no
//! recompile, no filesystem or RPC on the frame path — and adds zoom controls
//! plus `Ctrl`+wheel scaling. State mirrors `image_preview.rs` on purpose.

use crate::theme::ui_px;

use gpui::{KeyBinding, ScrollDelta, ScrollWheelEvent, actions};

use super::*;

actions!(
    fintwind_diagram_preview,
    [
        DismissDiagramPreview,
        DiagramZoomIn,
        DiagramZoomOut,
        DiagramZoomReset
    ]
);

const DIAGRAM_PREVIEW_CONTEXT: &str = "DiagramPreview";
const DIAGRAM_PREVIEW_ANIMATION_DURATION: Duration = Duration::from_millis(140);
const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 4.0;
const ZOOM_STEP: f32 = 1.25;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(
            "escape",
            DismissDiagramPreview,
            Some(DIAGRAM_PREVIEW_CONTEXT),
        ),
        KeyBinding::new("ctrl-=", DiagramZoomIn, Some(DIAGRAM_PREVIEW_CONTEXT)),
        KeyBinding::new("ctrl--", DiagramZoomOut, Some(DIAGRAM_PREVIEW_CONTEXT)),
        KeyBinding::new("ctrl-0", DiagramZoomReset, Some(DIAGRAM_PREVIEW_CONTEXT)),
    ]);
}

pub(super) struct DiagramPreviewState {
    image: Arc<gpui::RenderImage>,
    base_width: f32,
    base_height: f32,
    zoom: f32,
    focus: FocusHandle,
    close_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    generation: u64,
}

impl Fintwind {
    pub(super) fn open_diagram_preview(
        &mut self,
        request: crate::md::diagram::PreviewRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One modal at a time: a diagram replaces an attachment preview.
        self.image_preview = None;
        self.diagram_preview_generation =
            self.diagram_preview_generation.wrapping_add(1);
        let generation = self.diagram_preview_generation;
        let focus = cx.focus_handle();
        self.diagram_preview = Some(DiagramPreviewState {
            image: request.image,
            base_width: request.width.max(1.0),
            base_height: request.height.max(1.0),
            zoom: 1.0,
            focus: focus.clone(),
            close_focus: cx.focus_handle(),
            previous_focus: window.focused(cx),
            generation,
        });

        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                let mut should_focus = false;
                let _ = weak.update(cx, |this, _| {
                    should_focus = this
                        .diagram_preview
                        .as_ref()
                        .is_some_and(|preview| preview.generation == generation);
                });
                if should_focus {
                    window.focus(&focus, cx);
                }
            });
        });
        cx.notify();
    }

    fn close_diagram_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = self.diagram_preview.take() else {
            return;
        };
        self.diagram_preview_generation =
            self.diagram_preview_generation.wrapping_add(1);
        if let Some(previous_focus) = preview.previous_focus {
            window.focus(&previous_focus, cx);
        } else {
            let focus = self.composer_focus(cx);
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn zoom_diagram_preview(&mut self, factor: f32, cx: &mut Context<Self>) {
        if let Some(preview) = self.diagram_preview.as_mut() {
            preview.zoom = (preview.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
            cx.notify();
        }
    }

    fn reset_diagram_zoom(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = self.diagram_preview.as_mut() {
            preview.zoom = 1.0;
            cx.notify();
        }
    }

    pub(super) fn render_diagram_preview(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let preview = self.diagram_preview.as_ref()?;
        let theme = Theme::current(cx);
        let image_source = preview.image.clone();
        let zoom = preview.zoom;
        let focus = preview.focus.clone();
        let close_focus = preview.close_focus.clone();
        let generation = preview.generation;
        let display_width = (preview.base_width * zoom).clamp(1.0, 8192.0);
        let display_height = (preview.base_height * zoom).clamp(1.0, 8192.0);
        let percent = format!("{}%", (zoom * 100.0).round() as u32);

        let toolbar_button = |id: &'static str, label: SharedString, tooltip: String| {
            let tooltip: SharedString = tooltip.into();
            div()
                .id(id)
                .tab_index(0)
                .h(px(28.0))
                .min_w(px(28.0))
                .px(px(6.0))
                .flex_none()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .text_size(ui_px(12.0))
                .text_color(gpui::white().opacity(0.9))
                .hover(|style| style.bg(gpui::white().opacity(0.12)))
                .tooltip(Tooltip::text(tooltip))
                .child(label)
        };

        let zoom_out = toolbar_button("diagram-zoom-out", "-".into(), tr!("diagram.zoom_out"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.zoom_diagram_preview(1.0 / ZOOM_STEP, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.zoom_diagram_preview(1.0 / ZOOM_STEP, cx);
                    cx.stop_propagation();
                }
            }));
        let zoom_in = toolbar_button("diagram-zoom-in", "+".into(), tr!("diagram.zoom_in"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.zoom_diagram_preview(ZOOM_STEP, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.zoom_diagram_preview(ZOOM_STEP, cx);
                    cx.stop_propagation();
                }
            }));
        let reset = toolbar_button("diagram-zoom-reset", "1:1".into(), tr!("diagram.reset_zoom"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.reset_diagram_zoom(cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.reset_diagram_zoom(cx);
                    cx.stop_propagation();
                }
            }));
        let close = toolbar_button(
            "diagram-zoom-close",
            "".into(),
            tr!("diagram.close_preview"),
        )
            .child(icon("icons/x.svg", 12.0, gpui::white().opacity(0.9)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.close_diagram_preview(window, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.close_diagram_preview(window, cx);
                    cx.stop_propagation();
                }
            }));

        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .px(px(6.0))
            .py(px(4.0))
            .rounded_full()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.48))
            .child(zoom_out)
            .child(
                div()
                    .min_w(px(52.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(ui_px(11.5))
                    .text_color(gpui::white().opacity(0.9))
                    .child(percent),
            )
            .child(zoom_in)
            .child(reset)
            .child(close);

        let image = div()
            .id("diagram-preview-scroll")
            .flex_1()
            .w_full()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .overflow_scroll()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_scroll_wheel(cx.listener(
                |this, event: &ScrollWheelEvent, _, cx| {
                    let control = event.modifiers.control || event.modifiers.platform;
                    if !control {
                        return;
                    }
                    let dy = match event.delta {
                        ScrollDelta::Pixels(point) => f32::from(point.y),
                        ScrollDelta::Lines(point) => point.y * 20.0,
                    };
                    if dy == 0.0 {
                        return;
                    }
                    let factor = if dy > 0.0 { 1.1 } else { 1.0 / 1.1 };
                    this.zoom_diagram_preview(factor, cx);
                    cx.stop_propagation();
                },
            ))
            .child(
                img(image_source)
                    .w(px(display_width))
                    .h(px(display_height))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(gpui::hsla(0.0, 0.0, 1.0, 0.1))
                    .shadow_lg()
                    .with_fallback(move || {
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(ui_px(12.0))
                            .text_color(gpui::white().opacity(0.78))
                            .child(icon(
                                "icons/alert.svg",
                                18.0,
                                gpui::white().opacity(0.78),
                            ))
                            .child(tr_cow!("attachments.preview_unavailable"))
                            .into_any_element()
                    }),
            );

        let content = div()
            .relative()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .child(toolbar)
            .child(image)
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });

        let close_button = div()
            .id("diagram-preview-close")
            .absolute()
            .top(px(14.0))
            .right(px(14.0))
            .track_focus(&close_focus)
            .tab_index(0)
            .size(px(32.0))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.48))
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(gpui::hsla(0.0, 0.0, 0.0, 0.66)))
            .active(|style| style.opacity(0.8))
            .tooltip(Tooltip::text(tr!("diagram.close_preview")))
            .child(icon("icons/x.svg", 13.0, gpui::white()))
            .on_click(cx.listener(|this, _, window, cx| {
                this.close_diagram_preview(window, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.close_diagram_preview(window, cx);
                    cx.stop_propagation();
                }
            }));

        let layer = div()
            .id(SharedString::from(format!(
                "diagram-preview-layer-{generation}"
            )))
            .absolute()
            .inset_0()
            .occlude()
            .track_focus(&focus)
            .key_context(DIAGRAM_PREVIEW_CONTEXT)
            .on_action(cx.listener(|this, _: &DismissDiagramPreview, window, cx| {
                this.close_diagram_preview(window, cx);
            }))
            .on_action(cx.listener(|this, _: &DiagramZoomIn, _, cx| {
                this.zoom_diagram_preview(ZOOM_STEP, cx);
            }))
            .on_action(cx.listener(|this, _: &DiagramZoomOut, _, cx| {
                this.zoom_diagram_preview(1.0 / ZOOM_STEP, cx);
            }))
            .on_action(cx.listener(|this, _: &DiagramZoomReset, _, cx| {
                this.reset_diagram_zoom(cx);
            }))
            .tab_group()
            .tab_stop(false)
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.82))
            .p(px(36.0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.close_diagram_preview(window, cx);
                }),
            )
            .child(content)
            .child(close_button)
            .with_animation(
                SharedString::from(format!("diagram-preview-enter-{generation}")),
                Animation::new(DIAGRAM_PREVIEW_ANIMATION_DURATION)
                    .with_easing(ease_out_quint()),
                |element, delta| element.opacity(delta),
            );

        Some(gpui::deferred(layer).with_priority(5).into_any_element())
    }
}
