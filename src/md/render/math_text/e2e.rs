//! Headless Markdown -> worker -> layout -> input -> source selection E2E.
//! No real application, provider, daemon or screenshots are involved.

use super::*;
use gpui::{Context, Render, ScrollDelta, ScrollWheelEvent, TestAppContext};

struct Harness {
    markdown: MarkdownView,
    selection: TranscriptSelection,
    width: f32,
    font_size: f32,
    vertical_events: Rc<Cell<usize>>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&Theme::dark());
        let metrics = Metrics {
            text_size: self.font_size,
            line_height: self.font_size * 1.5,
            ..Metrics::BODY
        };
        let ctx = Ctx::new("math-layout-e2e", &palette, metrics, self.selection.clone());
        let vertical_events = self.vertical_events.clone();
        div()
            .w(px(self.width))
            .on_scroll_wheel(move |event, _, _| {
                if event.delta.pixel_delta(px(20.0)).y != px(0.0) {
                    vertical_events.set(vertical_events.get() + 1);
                }
            })
            .child(frame_reset(self.selection.clone()))
            .children(markdown(&self.markdown, &ctx))
    }
}

#[gpui::test]
fn math_layout_readability_and_overflow_e2e(cx: &mut TestAppContext) {
    // Failure modes: formulas inherit an optically tiny size; a narrow viewport
    // shrinks glyphs; formula scrolling moves prose or consumes vertical wheel
    // events; keyboard cannot reach the end; selection ignores the scroll;
    // tall glyphs collide with adjacent rows; settled frames redo layout.
    let source = concat!(
        "Before $x^2$ after\n\n",
        "$$\\sum_{n=1}^{\\infty} \\frac{(-1)^{n+1} x^n}{n^2}",
        " + \\int_0^1 \\frac{\\ln(1+xt)}{t}\\,dt",
        " + \\sqrt{\\frac{1+x^2}{1-x^2}}$$\n\n",
        "After $\\frac{1}{\\sqrt{1+x^2}}$ text\n\n",
        "Prefix $\\sum_{n=1}^{\\infty} \\frac{(-1)^{n+1} x^n}{n^2}",
        " + \\int_0^1 \\frac{\\ln(1+xt)}{t}\\,dt$ suffix"
    );
    let (view, cx) = cx.add_window_view(|_, _| {
        let mut markdown = MarkdownView::new();
        markdown.set_text(source, false);
        Harness {
            markdown,
            selection: TranscriptSelection::default(),
            width: 180.0,
            font_size: 14.0,
            vertical_events: Rc::default(),
        }
    });
    cx.run_until_parked();
    let geometry = view.read_with(cx, |view, _| {
        let registry = view.selection.registry.borrow();
        let TextGeometry::Math { geometry, .. } = &registry.entries()[1].geometry else {
            panic!("display formula must use native math")
        };
        geometry.clone()
    });
    let (natural_width, natural_height, position, range) = {
        let frame = geometry.0.borrow();
        let (bounds, flow) = frame.as_ref().unwrap();
        let fragment = &flow.fragments[0];
        assert!(fragment.token.width > f32::from(bounds.size.width));
        (
            fragment.token.width,
            fragment.token.ascent + fragment.token.descent,
            bounds.origin + point(bounds.size.width / 2.0, fragment.origin.y + px(5.0)),
            fragment.token.range.clone(),
        )
    };
    assert_eq!(geometry.index_for_position(position), Ok(range.start));
    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(-10000.0), px(0.0))),
        ..Default::default()
    });
    cx.run_until_parked();
    // The right edge is now visible, at unchanged glyph size.
    let scrolled = view.read_with(cx, |view, _| {
        let registry = view.selection.registry.borrow();
        let TextGeometry::Math { geometry, .. } = &registry.entries()[1].geometry else {
            panic!()
        };
        assert_eq!(geometry.index_for_position(position), Ok(range.end));
        let rects = geometry.range_rects(&range);
        assert!(rects.iter().all(|rect| rect.size.width <= px(180.0)));
        geometry.clone()
    });
    cx.update(|window, cx| window.focus_next(cx));
    // First focusable paragraph contains the short inline formula.
    cx.update(|window, cx| window.focus_next(cx));
    cx.simulate_keystrokes("home");
    cx.run_until_parked();
    assert_eq!(scrolled.index_for_position(position), Ok(range.start));
    cx.simulate_keystrokes("end");
    cx.run_until_parked();
    assert_eq!(scrolled.index_for_position(position), Ok(range.end));

    let (track_left, track_right) = {
        let frame = scrolled.0.borrow();
        let (bounds, flow) = frame.as_ref().unwrap();
        let fragment = &flow.fragments[0];
        let y = bounds.top() + px(fragment.row_top + fragment.row_height - SCROLL_LANE / 2.0);
        (
            point(bounds.left() + px(1.0), y),
            point(bounds.right() - px(1.0), y),
        )
    };
    cx.simulate_mouse_down(track_right, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_event(MouseMoveEvent {
        position: track_left,
        pressed_button: Some(MouseButton::Left),
        ..Default::default()
    });
    cx.simulate_mouse_up(track_left, MouseButton::Left, gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(scrolled.index_for_position(position), Ok(range.start));
    cx.simulate_keystrokes("end");
    cx.run_until_parked();
    assert_eq!(scrolled.index_for_position(position), Ok(range.end));

    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-30.0))),
        ..Default::default()
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| assert_eq!(view.vertical_events.get(), 1));
    assert_eq!(scrolled.index_for_position(position), Ok(range.end));

    let (inline_geometry, prose_before) = view.read_with(cx, |view, _| {
        let registry = view.selection.registry.borrow();
        let TextGeometry::Math { geometry, .. } = &registry.entries()[3].geometry else {
            panic!()
        };
        let frame = geometry.0.borrow();
        let (_, flow) = frame.as_ref().unwrap();
        let prose = flow
            .fragments
            .iter()
            .filter_map(|fragment| {
                matches!(fragment.token.content, Content::Text(_)).then_some(fragment.origin)
            })
            .collect::<Vec<_>>();
        for fragment in &flow.fragments {
            assert!(fragment.origin.y >= px(fragment.row_top));
            assert!(
                fragment.origin.y + px(fragment.token.ascent + fragment.token.descent)
                    <= px(fragment.row_top + fragment.row_height)
            );
        }
        (geometry.clone(), prose)
    });
    let inline_position = {
        let frame = inline_geometry.0.borrow();
        let (bounds, flow) = frame.as_ref().unwrap();
        let fragment = flow
            .fragments
            .iter()
            .find(|fragment| matches!(fragment.token.content, Content::Formula(_)))
            .unwrap();
        bounds.origin + fragment.origin + point(px(10.0), px(5.0))
    };
    cx.simulate_event(ScrollWheelEvent {
        position: inline_position,
        delta: ScrollDelta::Pixels(point(px(-80.0), px(0.0))),
        ..Default::default()
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        let registry = view.selection.registry.borrow();
        let TextGeometry::Math { geometry, .. } = &registry.entries()[3].geometry else {
            panic!()
        };
        let frame = geometry.0.borrow();
        let (_, flow) = frame.as_ref().unwrap();
        let original = inline_geometry.0.borrow();
        assert!(
            Rc::ptr_eq(flow, &original.as_ref().unwrap().1),
            "scrolling must reuse placed glyphs"
        );
        let prose = flow
            .fragments
            .iter()
            .filter_map(|fragment| {
                matches!(fragment.token.content, Content::Text(_)).then_some(fragment.origin)
            })
            .collect::<Vec<_>>();
        assert_eq!(prose, prose_before, "only the formula may scroll");
    });

    view.update(cx, |view, cx| {
        view.width = 700.0;
        cx.notify();
    });
    cx.run_until_parked();
    let (wide_width, wide_height) = view.read_with(cx, |view, _| {
        let registry = view.selection.registry.borrow();
        let TextGeometry::Math { geometry, .. } = &registry.entries()[1].geometry else {
            panic!()
        };
        let frame = geometry.0.borrow();
        let (_, flow) = frame.as_ref().unwrap();
        let fragment = &flow.fragments[0];
        assert!(fragment.origin.x > px(0.0));
        (
            fragment.token.width,
            fragment.token.ascent + fragment.token.descent,
        )
    });
    assert_eq!(natural_width, wide_width);
    assert_eq!(natural_height, wide_height);

    let baseline_error = view.read_with(cx, |view, _| {
        let registry = view.selection.registry.borrow();
        let TextGeometry::Math { geometry, .. } = &registry.entries()[0].geometry else {
            panic!()
        };
        let frame = geometry.0.borrow();
        let (_, flow) = frame.as_ref().unwrap();
        let baseline = flow.fragments[0].origin.y + px(flow.fragments[0].token.ascent);
        flow.fragments
            .iter()
            .map(|fragment| {
                f32::from(fragment.origin.y + px(fragment.token.ascent) - baseline).abs()
            })
            .fold(0.0f32, f32::max)
    });
    assert!(
        baseline_error < 0.01,
        "text and math must share an alphabetic baseline"
    );
    let raster_count = view.read_with(cx, |view, _| {
        let flats = view.markdown.flats.borrow();
        let mut count = 0;
        for data in flats.values().filter_map(|flat| flat.math.as_ref()) {
            let cache = data.cache.borrow();
            for rendered in cache.images.iter().flatten() {
                let dimensions = rendered.image.size(0);
                let width = dimensions.width.0 as usize;
                let height = dimensions.height.0 as usize;
                let bytes = rendered.image.as_bytes(0).unwrap();
                for y in 0..height {
                    for x in 0..width {
                        if x == 0 || y == 0 || x + 1 == width || y + 1 == height {
                            assert_eq!(
                                bytes[(y * width + x) * 4 + 3],
                                0,
                                "outlined glyphs must retain transparent raster padding"
                            );
                        }
                    }
                }
                count += 1;
            }
        }
        count
    });
    assert!(raster_count >= 4);
    view.update(cx, |view, cx| {
        view.font_size = 21.0;
        cx.notify();
    });
    cx.run_until_parked();
    let enlarged_width = view.read_with(cx, |view, _| {
        let registry = view.selection.registry.borrow();
        let TextGeometry::Math { geometry, .. } = &registry.entries()[1].geometry else {
            panic!()
        };
        geometry.0.borrow().as_ref().unwrap().1.fragments[0]
            .token
            .width
    });
    assert!(
        (enlarged_width / natural_width - 1.5).abs() < 0.05,
        "formula size must follow the surrounding text-size preference"
    );

    let output = std::env::var_os("FINTWIND_MATH_E2E_PROOF")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/math-layout-e2e")
                .join(uuid::Uuid::new_v4().to_string())
                .join("proof.json")
        });
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&serde_json::json!({
            "passed": true,
            "source": source,
            "narrow_viewport": 180,
            "wide_viewport": 700,
            "formula_width": natural_width,
            "formula_height": natural_height,
            "wheel_and_keyboard_scrolling": true,
            "scroll_track_dragging": true,
            "selection_tracks_visible_formula": true,
            "resize_preserves_glyph_size": true,
            "vertical_wheel_propagates": true,
            "inline_prose_stays_stationary": true,
            "scrolling_reuses_placed_glyphs": true,
            "baseline_error_px": baseline_error,
            "rasters_with_clear_ink_padding": raster_count,
            "enlarged_formula_width": enlarged_width
        }))
        .unwrap(),
    )
    .unwrap();
}
