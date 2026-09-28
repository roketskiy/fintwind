use super::*;

use super::sidebar::localized_session_title;

/// A press on a session tab, waiting to become either nothing (the release of
/// a plain click — the press itself already activated the tab) or a reorder.
#[derive(Clone, Copy, Debug)]
pub(super) struct SessionTabDrag {
    pub session_id: Uuid,
    start_x: f32,
    /// Whether the pointer has travelled far enough for the drag to start
    /// shuffling tabs. A click without it must never reorder the strip.
    pub active: bool,
}

/// Pointer travel before a press starts reordering tabs, so a jittery click
/// never shuffles the strip.
const SESSION_TAB_DRAG_THRESHOLD: f32 = 4.0;
const SESSION_TAB_HEIGHT: f32 = 30.0;
const SESSION_TAB_WIDTH: f32 = 190.0;
const SESSION_TAB_FADE_WIDTH: f32 = 16.0;

/// Which tab a drag lands on, given every tab's `(left, right)` span in the
/// strip and the pointer's x.
///
/// Moving left, the first tab whose midpoint the pointer crossed wins. Moving
/// right, the furthest crossed midpoint wins, so sweeping across several tabs
/// drops at the last one rather than the first. `None` when no boundary was
/// crossed.
fn drag_target_index(tab_spans: &[(f32, f32)], from: usize, pointer_x: f32) -> Option<usize> {
    let mut target = from;
    for (index, &(left, right)) in tab_spans.iter().enumerate() {
        let midpoint = (left + right) / 2.0;
        if index < from && pointer_x < midpoint {
            return Some(index);
        }
        if index > from && pointer_x > midpoint {
            target = index;
        }
    }
    (target != from).then_some(target)
}

/// Which tab activates after the one at `removed_index` closes: the tab that
/// took its place, else the previous one.
pub(super) fn tab_neighbor_after_close(remaining: &[Uuid], removed_index: usize) -> Option<Uuid> {
    remaining
        .get(removed_index)
        .or_else(|| removed_index.checked_sub(1).and_then(|index| remaining.get(index)))
        .copied()
}

#[derive(Clone, Copy)]
enum TabFadeSide {
    Left,
    Right,
}

fn tab_fade_visible(offset_x: Pixels, max_offset: Pixels, side: TabFadeSide) -> bool {
    let scrolled = -offset_x;
    let threshold = px(0.5);
    match side {
        TabFadeSide::Left => scrolled > threshold,
        TabFadeSide::Right => max_offset - scrolled > threshold,
    }
}

fn tab_fade(
    scroll_handle: ScrollHandle,
    side: TabFadeSide,
    surface: Hsla,
) -> impl IntoElement {
    canvas(
        move |bounds, _, _| {
            let visible = tab_fade_visible(
                scroll_handle.offset().x,
                scroll_handle.max_offset().x,
                side,
            );
            visible.then(|| {
                let transparent = surface.opacity(0.0);
                let background = match side {
                    TabFadeSide::Left => linear_gradient(
                        90.0,
                        linear_color_stop(surface, 0.0),
                        linear_color_stop(transparent, 1.0),
                    ),
                    TabFadeSide::Right => linear_gradient(
                        90.0,
                        linear_color_stop(transparent, 0.0),
                        linear_color_stop(surface, 1.0),
                    ),
                };
                fill(bounds, background)
            })
        },
        |_, fade, window, _| {
            if let Some(fade) = fade {
                window.paint_quad(fade);
            }
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .when(matches!(side, TabFadeSide::Left), |element| element.left_0())
    .when(matches!(side, TabFadeSide::Right), |element| element.right_0())
    .w(px(SESSION_TAB_FADE_WIDTH))
}

impl Fintwind {
    /// [`FintwindPane`] delegate for the header tab strip island.
    ///
    /// The strip is its own island so the working spinners it hosts lease the
    /// strip's view: a pulse tick rebuilds only the strip, never the whole
    /// window. Whatever header space the tabs leave over stays a drag region
    /// for the window.
    pub(super) fn session_tabs_pane_content(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let new_tab_button = div()
            .id("new-session-tab")
            .w(px(28.0))
            .h(px(28.0))
            .flex_none()
            .mr(px(4.0))
            .rounded(px(7.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .track_focus(&self.new_tab_focus)
            .tab_index(0)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .tooltip(Tooltip::text(tr!("tabs.new_session")))
            .child(icon("icons/plus.svg", 14.0, theme.text_tertiary))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.new_session_action(&NewSession, window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.new_session_action(&NewSession, window, cx);
                    cx.stop_propagation();
                }
            }));

        // A session awaiting activation reads as selected immediately, the
        // same contract the sidebar rows follow.
        let pending_activation = self.pending_session_activation.map(|pending| pending.session_id);
        let active_tab = pending_activation.or(self.state.selected_session);
        let mut strip = div()
            .id("session-tab-strip")
            .h_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(2.0))
            .overflow_x_scroll()
            .track_scroll(&self.session_tabs_scroll_handle);
        let session_ids = self.open_tabs.clone();
        for session_id in session_ids.into_iter() {
            strip = strip.child(self.render_session_tab(
                session_id,
                active_tab == Some(session_id),
                cx,
            ));
        }
        // Keeps the last tab clear of the right fade, as in the right panel.
        strip = strip.child(div().w(px(SESSION_TAB_FADE_WIDTH)).h(px(1.0)).flex_none());
        let reveal_pending = self.pending_session_tab_reveal;
        let scroll_handle = self.session_tabs_scroll_handle.clone();
        let reveal_fintwind = cx.entity().downgrade();
        let strip_host = div()
            .relative()
            .min_w_0()
            .flex_shrink(1.0)
            .h_full()
            .child(strip)
            .when_some(reveal_pending, |element, tab_index| {
                element.child(
                    canvas(
                        move |_, window, _| {
                            // Scroll just far enough that the activated tab
                            // clears the fades, then retire the request.
                            if let Some(item) = scroll_handle.bounds_for_item(tab_index) {
                                let viewport = scroll_handle.bounds();
                                let offset = scroll_handle.offset();
                                let inset = px(SESSION_TAB_FADE_WIDTH);
                                let mut safe_offset = offset.x;
                                let visible_left = item.left() + offset.x;
                                let visible_right = item.right() + offset.x;
                                if visible_left < viewport.left() + inset {
                                    safe_offset += viewport.left() + inset - visible_left;
                                } else if visible_right > viewport.right() - inset {
                                    safe_offset -= visible_right - (viewport.right() - inset);
                                }
                                let safe_offset =
                                    safe_offset.clamp(-scroll_handle.max_offset().x, px(0.0));
                                if safe_offset != offset.x {
                                    scroll_handle.set_offset(point(safe_offset, offset.y));
                                }
                            }
                            window.on_next_frame(move |_, cx| {
                                let _ = reveal_fintwind.update(cx, |this, cx| {
                                    if this.pending_session_tab_reveal == Some(tab_index) {
                                        this.pending_session_tab_reveal = None;
                                        cx.notify();
                                    }
                                });
                            });
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .child(tab_fade(
                self.session_tabs_scroll_handle.clone(),
                TabFadeSide::Left,
                theme.surface,
            ))
            .child(tab_fade(
                self.session_tabs_scroll_handle.clone(),
                TabFadeSide::Right,
                theme.surface,
            ));
        let drag_filler = self.window_drag_region(
            div()
                .id("header-tabs-drag-region")
                .h_full()
                .flex_1()
                .min_w(px(12.0)),
            cx,
        );
        div()
            .size_full()
            .flex()
            .items_center()
            .min_w_0()
            .child(new_tab_button)
            .child(strip_host)
            .child(drag_filler)
            .into_any_element()
    }

    fn render_session_tab(
        &self,
        session_id: Uuid,
        active: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let session = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id);
        let title = session
            .map(localized_session_title)
            .unwrap_or_else(|| tr!("session.new_task"));
        let busy = session.is_some_and(|session| session.status.is_busy());
        let waiting = session.is_some_and(|session| session.status == SessionStatus::Waiting);
        let unread = self.tab_unread.contains(&session_id);
        let focus = self
            .tab_focuses
            .borrow_mut()
            .entry(session_id)
            .or_insert_with(|| cx.focus_handle())
            .clone();
        let tab = div()
            .id(SharedString::from(format!("session-tab-{session_id}")))
            .h(px(SESSION_TAB_HEIGHT))
            .w(px(SESSION_TAB_WIDTH))
            .px(px(8.0))
            .rounded(px(7.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .track_focus(&focus)
            .tab_index(0)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .when(active, |element| element.bg(theme.overlay_strong))
            .when(!active, |element| {
                element
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
            })
            // A tab activates on press, like a browser tab; the same press
            // arms the drag that may reorder the strip.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.session_tab_drag = Some(SessionTabDrag {
                        session_id,
                        start_x: f32::from(event.position.x),
                        active: false,
                    });
                    this.select_session(session_id, cx);
                }),
            )
            .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.close_session_tab(session_id, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "enter" | "space" => {
                        this.select_session(session_id, cx);
                        cx.stop_propagation();
                    }
                    "left" => {
                        this.select_neighbor_session_tab(-1, cx);
                        cx.stop_propagation();
                    }
                    "right" => {
                        this.select_neighbor_session_tab(1, cx);
                        cx.stop_propagation();
                    }
                    "delete" => {
                        this.close_session_tab(session_id, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .when(busy, |element| {
                // A live turn in this session — foreground or background. The
                // spinner rides the pulse clock inside this island, so its
                // ticks never rebuild the rest of the window.
                element.child(
                    div()
                        .flex_none()
                        .child(motion::spin(icon(
                            "icons/loader-circle.svg",
                            12.0,
                            theme.accent,
                        ))),
                )
            })
            .when(!busy && waiting, |element| {
                element.child(
                    div()
                        .id(SharedString::from(format!("tab-waiting-{session_id}")))
                        .size(px(7.0))
                        .flex_none()
                        .rounded_full()
                        .bg(theme.warning)
                        .tooltip(Tooltip::text(tr!("tabs.waiting"))),
                )
            })
            .when(!busy && !waiting && unread, |element| {
                element.child(
                    div()
                        .id(SharedString::from(format!("tab-unread-{session_id}")))
                        .size(px(7.0))
                        .flex_none()
                        .rounded_full()
                        .bg(theme.accent)
                        .tooltip(Tooltip::text(tr!("tabs.unread"))),
                )
            })
            .child(
                div()
                    .id(SharedString::from(format!("session-tab-title-{session_id}")))
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .tooltip(Tooltip::text(title.clone()))
                    .text_size(ui_px(12.0))
                    .text_color(if active {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("close-session-tab-{session_id}")))
                    .w(px(18.0))
                    .h(px(18.0))
                    .mx(px(-2.0))
                    .flex_none()
                    .rounded(px(5.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|element| element.bg(theme.overlay_strong))
                    .active(|element| element.opacity(0.7))
                    .tooltip(Tooltip::text(tr!("tabs.close")))
                    .child(icon("icons/x.svg", 10.0, theme.text_tertiary))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.close_session_tab(session_id, cx);
                    })),
            );
        tab.into_any_element()
    }

    /// Window-level mouse move for an in-progress tab drag. Registered on the
    /// root so the drag keeps tracking when the pointer leaves the strip.
    pub(super) fn session_tab_drag_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.session_tab_drag.as_mut() else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.session_tab_drag = None;
            return;
        }
        let pointer_x = f32::from(event.position.x);
        if !drag.active {
            if (pointer_x - drag.start_x).abs() < SESSION_TAB_DRAG_THRESHOLD {
                return;
            }
            drag.active = true;
        }
        let Some(from) = self
            .open_tabs
            .iter()
            .position(|tab| *tab == drag.session_id)
        else {
            self.session_tab_drag = None;
            return;
        };
        let tab_spans = self
            .open_tabs
            .iter()
            .enumerate()
            .map(|(index, _)| {
                self.session_tabs_scroll_handle
                    .bounds_for_item(index)
                    .map(|bounds| (f32::from(bounds.left()), f32::from(bounds.right())))
            })
            .collect::<Option<Vec<_>>>();
        let Some(tab_spans) = tab_spans else {
            return;
        };
        if let Some(target) = drag_target_index(&tab_spans, from, pointer_x) {
            let session_id = self.open_tabs.remove(from);
            self.open_tabs.insert(target, session_id);
            // Reveal requests name a position, and the positions just moved.
            self.pending_session_tab_reveal = None;
            cx.notify();
        }
    }

    /// Window-level mouse up ending a tab drag. Activation already happened
    /// on mouse down, so the release itself has nothing more to do.
    pub(super) fn session_tab_drag_mouse_up(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.session_tab_drag = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{drag_target_index, tab_neighbor_after_close};
    use uuid::Uuid;

    fn spans(lefts: &[f32], width: f32) -> Vec<(f32, f32)> {
        lefts.iter().map(|left| (*left, left + width)).collect()
    }

    /// Tabs 90 wide at x = 0, 100, 200 → midpoints at 45, 145, 245.
    fn three_tabs() -> Vec<(f32, f32)> {
        spans(&[0.0, 100.0, 200.0], 90.0)
    }

    #[test]
    fn drag_moves_to_the_first_midpoint_crossed_leftward() {
        let tabs = three_tabs();
        // Dragging tab 2 left below tab 1's midpoint (145) crosses it.
        assert_eq!(drag_target_index(&tabs, 2, 144.0), Some(1));
        // Still right of that midpoint: nothing crossed.
        assert_eq!(drag_target_index(&tabs, 2, 146.0), None);
        // Sweeping far left lands at the leftmost crossed midpoint, not the last.
        assert_eq!(drag_target_index(&tabs, 2, 10.0), Some(0));
    }

    #[test]
    fn drag_moves_to_the_furthest_midpoint_crossed_rightward() {
        let tabs = three_tabs();
        // Past tab 1's midpoint only.
        assert_eq!(drag_target_index(&tabs, 0, 146.0), Some(1));
        // Sweeping past tab 2's midpoint (245) lands at the last, not the first.
        assert_eq!(drag_target_index(&tabs, 0, 246.0), Some(2));
    }

    #[test]
    fn a_click_that_never_crossed_a_midpoint_reorders_nothing() {
        let tabs = three_tabs();
        assert_eq!(drag_target_index(&tabs, 1, 145.0), None);
    }

    #[test]
    fn closing_a_tab_prefers_its_successor_then_its_predecessor() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        // Closing the middle tab of [a, b, c] activates its successor.
        assert_eq!(tab_neighbor_after_close(&[a, c], 1), Some(c));
        // Closing the first tab activates the one that took its place.
        assert_eq!(tab_neighbor_after_close(&[b, c], 0), Some(b));
        // Closing the last tab falls back to the previous one.
        assert_eq!(tab_neighbor_after_close(&[a, b], 2), Some(b));
        // Closing the only tab leaves nothing to activate.
        assert_eq!(tab_neighbor_after_close(&[], 0), None);
    }
}
