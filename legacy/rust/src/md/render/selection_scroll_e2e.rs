//! Headless interaction E2E using the real selectable text and virtual list.
//! No daemon, external process, real clipboard, or user data is involved.

use super::*;
use crate::app::TranscriptScrollTarget;
use gpui::{
    Context, FocusHandle, FollowMode, ListAlignment, ListState, Modifiers, Render, TestAppContext,
    list,
};
use std::time::Duration;

struct Harness {
    list: ListState,
    selection: TranscriptSelection,
    focus: FocusHandle,
    following: Rc<Cell<bool>>,
    scrolled: Rc<Cell<bool>>,
    tail_recheck: Rc<Cell<bool>>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let selection = self.selection.clone();
        let input_selection = selection.clone();
        let focus = self.focus.clone();
        let target = Rc::new(TranscriptScrollTarget::new(
            self.list.clone(),
            self.following.clone(),
            self.scrolled.clone(),
            self.tail_recheck.clone(),
        )) as Rc<dyn TranscriptSelectionScroll>;
        div()
            .relative()
            .w(px(400.0))
            .h(px(160.0))
            .track_focus(&self.focus)
            .child(frame_reset(selection.clone()))
            .child(
                list(self.list.clone(), move |index, _, _| {
                    let flat = flatten_plain(
                        format!("row {index:02} content"),
                        SANS_FAMILY,
                        FontWeight::NORMAL,
                        Theme::light().text,
                    );
                    div()
                        .h(px(28.0))
                        .w_full()
                        .flex()
                        .items_center()
                        .child(selectable_flat_text(
                            &flat,
                            TextKey::new(format!("row-{index:02}"), 0),
                            selection.clone(),
                            Theme::light().surface,
                            Theme::light().accent,
                            true,
                        ))
                        .into_any_element()
                })
                .size_full(),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        install_selection_input_with_focus_and_scroll(
                            window,
                            &input_selection,
                            &focus,
                            target.clone(),
                        );
                    },
                )
                .absolute()
                .size(px(0.0)),
            )
    }
}

fn advance_frames(cx: &mut gpui::VisualTestContext, count: usize) {
    for _ in 0..count {
        cx.background_executor
            .advance_clock(Duration::from_millis(17));
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
    }
}

fn logical_scroll_y(list: &ListState) -> f32 {
    let offset = list.logical_scroll_top();
    offset.item_ix as f32 * 28.0 + f32::from(offset.offset_in_item)
}

#[gpui::test]
fn selection_edge_scroll_e2e(cx: &mut TestAppContext) {
    // Failure modes: an unrelated drag scrolls the transcript; scrolling stops
    // with a stationary pointer; virtualized-away anchors stop selection; reverse
    // movement keeps stale spans; releasing the button keeps scrolling.
    let (view, cx) = cx.add_window_view(|_, cx| Harness {
        list: ListState::new(40, ListAlignment::Top, px(140.0)),
        selection: TranscriptSelection::default(),
        focus: cx.focus_handle(),
        following: Rc::new(Cell::new(true)),
        scrolled: Rc::new(Cell::new(false)),
        tail_recheck: Rc::new(Cell::new(false)),
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let (list, selection, following, scrolled, tail_recheck) = cx.read_entity(&view, |view, _| {
        (
            view.list.clone(),
            view.selection.clone(),
            view.following.clone(),
            view.scrolled.clone(),
            view.tail_recheck.clone(),
        )
    });
    let bounds = list.viewport_bounds();
    assert!(bounds.size.height > px(0.0));
    assert!(list.max_offset_for_scrollbar().y > px(0.5));

    let click = selection.registry.borrow().entries()[0]
        .geometry
        .bounds()
        .origin
        + point(px(4.0), px(8.0));
    for (click_count, expected) in [(2, "row"), (3, "row 00 content")] {
        cx.simulate_event(MouseDownEvent {
            position: click,
            button: MouseButton::Left,
            click_count,
            ..Default::default()
        });
        cx.simulate_mouse_up(click, MouseButton::Left, Modifiers::none());
        assert_eq!(
            selection.selection.borrow().selected_text().as_deref(),
            Some(expected),
            "double/triple click must retain its word/line selection"
        );
    }

    let unrelated = point(bounds.right() + px(30.0), bounds.bottom() - px(1.0));
    let before_unrelated = list.scroll_px_offset_for_scrollbar().y;
    cx.simulate_mouse_down(unrelated, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(unrelated, MouseButton::Left, Modifiers::none());
    advance_frames(cx, 5);
    cx.simulate_mouse_up(unrelated, MouseButton::Left, Modifiers::none());
    assert_eq!(
        list.scroll_px_offset_for_scrollbar().y,
        before_unrelated,
        "dragging outside the transcript must not scroll it"
    );

    let start = {
        let registry = selection.registry.borrow();
        let TextGeometry::Text { layout, .. } = &registry.entries()[0].geometry else {
            panic!("fixture should have a selectable text layout");
        };
        layout.position_for_index(0).unwrap() + point(px(0.5), px(8.0))
    };
    let edge = point(bounds.left() + px(250.0), bounds.bottom() - px(1.0));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    assert!(
        selection.selection.borrow().is_dragging(),
        "fixture must start a real text selection at {start:?}"
    );
    cx.simulate_mouse_move(edge, MouseButton::Left, Modifiers::none());
    let after_move = list.scroll_px_offset_for_scrollbar().y;
    advance_frames(cx, 90);
    let after_hold = list.scroll_px_offset_for_scrollbar().y;
    assert!(
        after_hold < after_move - px(100.0),
        "a stationary pointer at the edge must keep scrolling: {after_move:?} -> {after_hold:?}"
    );
    assert!(
        !following.get(),
        "manual scrolling must release the turn anchor"
    );
    assert!(
        scrolled.get(),
        "manual scrolling must update the scroll state"
    );
    assert!(
        !tail_recheck.get(),
        "tail following must wait until release"
    );
    let selected = selection
        .selection
        .borrow()
        .selected_text()
        .unwrap_or_default();
    let rows = selected
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert!(
        rows.len() >= 10,
        "selection must extend past one screen: {selected:?}"
    );
    for (index, row) in rows.iter().enumerate() {
        assert!(
            row.starts_with(&format!("row {index:02}")),
            "selection must retain every crossed row in order: {selected:?}"
        );
    }

    let reverse = point(bounds.left() + px(250.0), bounds.top() + px(1.0));
    cx.simulate_mouse_move(reverse, MouseButton::Left, Modifiers::none());
    advance_frames(cx, 45);
    let after_reverse = list.scroll_px_offset_for_scrollbar().y;
    let shorter = selection
        .selection
        .borrow()
        .selected_text()
        .unwrap_or_default();
    assert!(after_reverse > after_hold, "reversing must scroll back up");
    assert!(
        shorter.len() < selected.len(),
        "reversing must contract the selection"
    );

    cx.simulate_mouse_up(reverse, MouseButton::Left, Modifiers::none());
    assert!(
        tail_recheck.get(),
        "release must recheck whether the tail was reached"
    );
    advance_frames(cx, 15);
    assert_eq!(
        list.scroll_px_offset_for_scrollbar().y,
        after_reverse,
        "releasing must stop scrolling"
    );

    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/selection-scroll-e2e")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(
        output.join("proof.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "passed": true,
            "initialOffset": f32::from(before_unrelated),
            "afterHold": f32::from(after_hold),
            "afterReverse": f32::from(after_reverse),
            "selectedRows": rows.len(),
            "selectedText": selected,
            "afterReverseText": shorter,
        }))
        .unwrap(),
    )
    .unwrap();
    eprintln!("selection scroll E2E proof: {}", output.display());
}

#[gpui::test]
fn selection_edge_scroll_bottom_up_e2e(cx: &mut TestAppContext) {
    // Failure modes: a tail pin defeats a small upward step; the upper edge
    // loses the anchor after virtualization; returning down does not shrink
    // the selected text; event frequency changes the scrolling speed.
    let (view, cx) = cx.add_window_view(|_, cx| {
        let list = ListState::new(40, ListAlignment::Bottom, px(140.0));
        list.set_follow_mode(FollowMode::Tail);
        Harness {
            list,
            selection: TranscriptSelection::default(),
            focus: cx.focus_handle(),
            following: Rc::new(Cell::new(true)),
            scrolled: Rc::new(Cell::new(false)),
            tail_recheck: Rc::new(Cell::new(false)),
        }
    });
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let (list, selection, following, tail_recheck) = cx.read_entity(&view, |view, _| {
        (
            view.list.clone(),
            view.selection.clone(),
            view.following.clone(),
            view.tail_recheck.clone(),
        )
    });
    let bounds = list.viewport_bounds();
    let start = {
        let registry = selection.registry.borrow();
        let entry = registry
            .entries()
            .iter()
            .rev()
            .find(|entry| {
                let text_bounds = entry.geometry.bounds();
                text_bounds.bottom() <= bounds.bottom() && text_bounds.top() >= bounds.top()
            })
            .expect("a visible text row at the tail");
        let TextGeometry::Text { layout, .. } = &entry.geometry else {
            panic!("fixture should have a selectable text layout");
        };
        layout.position_for_index(entry.text.len()).unwrap() + point(px(0.5), px(8.0))
    };
    assert!(list.is_following_tail());
    let before = list.scroll_px_offset_for_scrollbar().y;
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    let shallow_edge = point(bounds.left() + px(1.0), bounds.top() + px(23.0));
    cx.simulate_mouse_move(shallow_edge, MouseButton::Left, Modifiers::none());
    advance_frames(cx, 1);
    let after_small_step = list.scroll_px_offset_for_scrollbar().y;
    assert!(
        after_small_step > before,
        "a subpixel upward step must release the pinned tail: {before:?} -> {after_small_step:?}"
    );
    assert!(!list.is_following_tail());
    assert!(!following.get());

    let edge = point(bounds.left() + px(1.0), bounds.top() + px(1.0));
    cx.simulate_mouse_move(edge, MouseButton::Left, Modifiers::none());
    let before_hold = logical_scroll_y(&list);
    advance_frames(cx, 30);
    let after_quiet = logical_scroll_y(&list);
    for _ in 0..30 {
        for _ in 0..8 {
            cx.simulate_mouse_move(edge, MouseButton::Left, Modifiers::none());
        }
        advance_frames(cx, 1);
    }
    let after_frequent = logical_scroll_y(&list);
    let quiet_distance = before_hold - after_quiet;
    let frequent_distance = after_quiet - after_frequent;
    assert!(quiet_distance > 100.0);
    assert!(
        (quiet_distance - frequent_distance).abs() < 2.0,
        "mouse event frequency must not change speed: {quiet_distance} vs {frequent_distance}"
    );
    let selected = selection
        .selection
        .borrow()
        .selected_text()
        .unwrap_or_default();
    let rows = selected
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            assert!(
                line.starts_with("row "),
                "incomplete selected row: {line:?}"
            );
            line[4..6].parse::<usize>().unwrap()
        })
        .collect::<Vec<_>>();
    assert!(
        rows.len() >= 10,
        "selection must cross multiple screens: {selected:?}"
    );
    assert!(rows.windows(2).all(|pair| pair[1] == pair[0] + 1));
    let reverse = point(bounds.left() + px(1.0), bounds.bottom() - px(1.0));
    cx.simulate_mouse_move(reverse, MouseButton::Left, Modifiers::none());
    advance_frames(cx, 30);
    let after_reverse = logical_scroll_y(&list);
    let shorter = selection
        .selection
        .borrow()
        .selected_text()
        .unwrap_or_default();
    assert!(after_reverse > after_frequent);
    assert!(shorter.len() < selected.len());
    cx.simulate_mouse_up(reverse, MouseButton::Left, Modifiers::none());
    assert!(tail_recheck.get());
    advance_frames(cx, 10);
    assert_eq!(logical_scroll_y(&list), after_reverse);

    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/selection-scroll-e2e")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(
        output.join("proof.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "passed": true,
            "scenario": "bottom_up_from_tail",
            "initialOffset": f32::from(before),
            "afterSmallStep": f32::from(after_small_step),
            "quietDistance": quiet_distance,
            "frequentEventDistance": frequent_distance,
            "selectedRows": rows,
            "selectedText": selected,
            "afterReverseText": shorter,
        }))
        .unwrap(),
    )
    .unwrap();
    eprintln!("bottom-up selection E2E proof: {}", output.display());
}
