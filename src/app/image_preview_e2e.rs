//! Native GPUI end-to-end tests for the unified image and Mermaid preview modal.

use std::cell::Cell;
use std::io::Cursor;
use std::rc::Rc;

use gpui::{
    ClipboardEntry, Context, Entity, FocusHandle, Modifiers, MouseButton, Render, RenderImage,
    StyleRefinement, TestAppContext, Window, div, px,
};

use super::image_preview::{
    IMAGE_PREVIEW_LAYER_PRIORITY, ImagePreviewCallbacks, ImagePreviewItem, ImagePreviewModal,
    ImagePreviewZoomMode,
};
use super::*;

struct CachedWorkspaceIsland {
    renders: Rc<Cell<usize>>,
}

impl Render for CachedWorkspaceIsland {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        div().size_full()
    }
}

struct PreviewE2eHarness {
    preview: Option<Entity<ImagePreviewModal>>,
    generation: u64,
    trigger_focus: FocusHandle,
    notifications: Vec<(String, bool)>,
    items: Vec<ImagePreviewItem>,
    activity_image_urls: Vec<String>,
    cached_island: Entity<CachedWorkspaceIsland>,
}

impl PreviewE2eHarness {
    fn open_items(
        &mut self,
        items: Vec<ImagePreviewItem>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let previous_focus = window.focused(cx);
        let weak_close = cx.entity().downgrade();
        let weak_notify = cx.entity().downgrade();
        let callbacks = ImagePreviewCallbacks {
            on_close: Rc::new(move |previous, window, cx| {
                let _ = weak_close.update(cx, |this, cx| {
                    this.preview = None;
                    if let Some(previous) = previous {
                        window.focus(&previous, cx);
                    }
                    cx.notify();
                });
            }),
            on_notify: Rc::new(move |message, success, cx| {
                let _ = weak_notify.update(cx, |this, cx| {
                    this.notifications.push((message, success));
                    cx.notify();
                });
            }),
            on_reveal: Rc::new(|_, _| {}),
        };
        let modal = cx.new(|cx| {
            ImagePreviewModal::new(items, index, generation, previous_focus, callbacks, cx)
        });
        let focus = modal.read(cx).focus_handle();
        let modal_id = modal.entity_id();
        self.preview = Some(modal);

        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                let mut should_focus = false;
                let _ = weak.update(cx, |this, _| {
                    should_focus = this
                        .preview
                        .as_ref()
                        .is_some_and(|current| current.entity_id() == modal_id);
                });
                if should_focus {
                    window.focus(&focus, cx);
                }
            });
        });
        cx.notify();
    }
}

impl Render for PreviewE2eHarness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let preview_layer = self.preview.as_ref().map(|modal| {
            gpui::deferred(div().absolute().inset_0().child(modal.clone()))
                .with_priority(IMAGE_PREVIEW_LAYER_PRIORITY)
        });
        let first_url = self.activity_image_urls.first().cloned().unwrap_or_default();
        let activity_img = (!first_url.is_empty()).then(|| {
            let decoded = super::transcript_view::decode_activity_image(&first_url);
            div()
                .id("e2e-activity-image-trigger")
                .debug_selector(|| "e2e-activity-image-trigger".into())
                .w(px(120.0))
                .h(px(60.0))
                .tab_index(0)
                .on_click(cx.listener(|this, _, window, cx| {
                    let items = this.items.clone();
                    this.open_items(items, 0, window, cx);
                }))
                .when_some(decoded, |box_el, image| {
                    box_el.child(gpui::img(image).size_full())
                })
        });
        div()
            .relative()
            .w(px(560.0))
            .h(px(420.0))
            .child(
                self.cached_island
                    .clone()
                    .cached(StyleRefinement::default().size_full()),
            )
            .child(
                div()
                    .id("e2e-open-trigger")
                    .debug_selector(|| "e2e-open-trigger".into())
                    .w(px(100.0))
                    .h(px(32.0))
                    .track_focus(&self.trigger_focus)
                    .tab_index(0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let items = this.items.clone();
                        this.open_items(items, 0, window, cx);
                    })),
            )
            .children(activity_img)
            .children(preview_layer)
    }
}

fn make_png_bytes(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let buffer = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
    let mut out = Vec::new();
    buffer
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode test PNG");
    out
}

fn make_render_image(width: u32, height: u32, rgba: [u8; 4]) -> Arc<RenderImage> {
    let mut buffer = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

fn init_test_app(cx: &mut TestAppContext) {
    cx.set_assets(ely_gpui_component::Assets::before(crate::assets::Assets));
    cx.update(|cx| {
        crate::assets::register_fonts(cx).expect("register bundled fonts");
        gpui_kit::init(cx);
        ely_gpui_component::init(cx).expect("initialize Ely");
        crate::ely::init(cx);
        crate::input::init(cx);
        crate::ui::menu::init(cx);
        super::image_preview::init(cx);
        crate::theme::init(cx);
    });
}

fn pump_frames(cx: &mut gpui::VisualTestContext, count: usize) {
    for _ in 0..count {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        });
    }
}

#[gpui::test]
fn image_preview_gallery_zoom_pan_rotate_copy_and_focus_e2e(cx: &mut TestAppContext) {
    // Failures: preview never grabs focus or fails to restore focus on Escape;
    // multi-image navigation misses bounds or resets incorrectly;
    // zoom / 1:1 / Fit fails to compute sizes or pan via drag;
    // dragging repaints cached workspace islands on every mouse move;
    // background rotation fails to swap dimensions; copy omits image clipboard entry.
    init_test_app(cx);

    let wide_png = make_png_bytes(800, 500, [220, 40, 40, 255]);
    let tall_png = make_png_bytes(80, 160, [40, 90, 220, 255]);
    let data_url = {
        use base64::Engine as _;
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&wide_png)
        )
    };
    let items = vec![
        ImagePreviewItem::from_image(
            Arc::new(gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                wide_png.clone(),
            )),
            "wide-capture.png".into(),
            None,
            false,
        ),
        ImagePreviewItem::from_image(
            Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, tall_png)),
            "tall-capture.png".into(),
            None,
            false,
        ),
    ];

    let island_renders = Rc::new(Cell::new(0));
    let island_renders_handle = island_renders.clone();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let trigger_focus = cx.focus_handle();
        window.focus(&trigger_focus, cx);
        let cached_island = cx.new(|_| CachedWorkspaceIsland {
            renders: island_renders_handle,
        });
        PreviewE2eHarness {
            preview: None,
            generation: 0,
            trigger_focus,
            notifications: Vec::new(),
            items,
            activity_image_urls: vec![data_url],
            cached_island,
        }
    });

    pump_frames(cx, 2);

    // 1. Click activity trigger to open preview, verify initial state & intrinsic dimensions.
    let trigger_bounds = cx
        .debug_bounds("e2e-activity-image-trigger")
        .expect("activity trigger bounds");
    cx.simulate_click(trigger_bounds.center(), Modifiers::none());
    pump_frames(cx, 4);

    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().expect("preview should open").read(cx);
        assert_eq!(modal.current_index(), 0);
        assert_eq!(modal.oriented_dimensions(), Some((800, 500)));
        assert!(matches!(modal.zoom_mode(), ImagePreviewZoomMode::Fit));
    });

    // 2. Navigate between slides with keyboard and toolbar buttons.
    cx.simulate_keystrokes("right");
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().unwrap().read(cx);
        assert_eq!(modal.current_index(), 1);
        assert_eq!(modal.oriented_dimensions(), Some((80, 160)));
    });

    let prev_bounds = cx.debug_bounds("image-preview-prev").expect("prev button");
    cx.simulate_click(prev_bounds.center(), Modifiers::none());
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().unwrap().read(cx);
        assert_eq!(modal.current_index(), 0);
    });

    // 3. Switch to 1:1 (800x500 overflows the 560x420 test window) and drag to pan.
    cx.simulate_keystrokes("ctrl-1");
    pump_frames(cx, 3);
    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().unwrap().read(cx);
        assert!(matches!(
            modal.zoom_mode(),
            ImagePreviewZoomMode::Scale(scale) if (scale - 1.0).abs() < 0.001
        ));
    });

    let stage_bounds = cx
        .debug_bounds("image-preview-stage")
        .expect("visible preview stage");
    let drag_start = stage_bounds.center();
    cx.simulate_mouse_down(drag_start, MouseButton::Left, Modifiers::none());
    pump_frames(cx, 1);
    let renders_during_drag_start = island_renders.get();
    for step in 1..=5 {
        let pos = gpui::point(
            drag_start.x - px(8.0 * step as f32),
            drag_start.y - px(6.0 * step as f32),
        );
        cx.simulate_mouse_move(pos, MouseButton::Left, Modifiers::none());
        pump_frames(cx, 1);
    }
    assert_eq!(
        island_renders.get(),
        renders_during_drag_start,
        "dragging zoomed image must not rebuild cached workspace panes"
    );
    let drag_end = gpui::point(drag_start.x - px(40.0), drag_start.y - px(30.0));
    cx.simulate_mouse_up(drag_end, MouseButton::Left, Modifiers::none());
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, cx| {
        let modal = harness
            .preview
            .as_ref()
            .expect("drag must not close preview")
            .read(cx);
        let offset = modal.scroll_offset();
        assert!(
            offset.x < px(-1.0) && offset.y < px(-1.0),
            "expected scroll offset to pan negatively, got {offset:?}"
        );
    });

    // Reset to Fit with Ctrl+0.
    cx.simulate_keystrokes("ctrl-0");
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().unwrap().read(cx);
        assert!(matches!(modal.zoom_mode(), ImagePreviewZoomMode::Fit));
    });

    // 4. Rotate clockwise 90 deg (`r`), wait for background PNG encode, verify swapped dimensions.
    cx.simulate_keystrokes("r");
    pump_frames(cx, 6);
    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().unwrap().read(cx);
        assert_eq!(modal.current_rotation(), 1);
        assert_eq!(modal.oriented_dimensions(), Some((500, 800)));
    });

    // Rotate counter-clockwise (`shift-r`) back to 0 deg.
    cx.simulate_keystrokes("shift-r");
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().unwrap().read(cx);
        assert_eq!(modal.current_rotation(), 0);
        assert_eq!(modal.oriented_dimensions(), Some((800, 500)));
    });

    // 5. Copy image to clipboard via Ctrl+C.
    cx.simulate_keystrokes("ctrl-c");
    pump_frames(cx, 2);
    let clipboard = cx
        .read_from_clipboard()
        .expect("clipboard should hold copied image");
    assert!(
        clipboard
            .entries()
            .iter()
            .any(|entry| matches!(entry, ClipboardEntry::Image(img) if !img.bytes.is_empty())),
        "expected image clipboard entry"
    );
    cx.read_entity(&view, |harness, _| {
        assert!(
            harness
                .notifications
                .iter()
                .any(|(msg, ok)| *ok && !msg.is_empty()),
            "expected success notification after copy"
        );
    });

    // 6. Focus the open trigger first, reopen, then verify Escape restores focus back to it.
    let close_bounds = cx.debug_bounds("image-preview-close").expect("close button");
    cx.simulate_click(close_bounds.center(), Modifiers::none());
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, _| assert!(harness.preview.is_none()));

    let open_trigger = cx.debug_bounds("e2e-open-trigger").expect("open trigger");
    cx.simulate_click(open_trigger.center(), Modifiers::none());
    pump_frames(cx, 4);
    cx.read_entity(&view, |harness, _| assert!(harness.preview.is_some()));

    cx.simulate_keystrokes("escape");
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, _| {
        assert!(harness.preview.is_none(), "Escape should dismiss preview");
    });
    cx.update(|window, cx| {
        let trigger_focus = view.read(cx).trigger_focus.clone();
        assert!(
            trigger_focus.is_focused(window),
            "closing preview should restore focus to the triggering control"
        );
    });
}

#[gpui::test]
fn image_preview_hosts_mermaid_diagram_zoom_pan_rotate_and_copy_e2e(cx: &mut TestAppContext) {
    init_test_app(cx);
    let diagram = make_render_image(1200, 800, [24, 140, 210, 255]);
    let diagram_item = ImagePreviewItem::from_diagram(
        crate::md::diagram::PreviewRequest {
            image: diagram,
            width: 600.0,
            height: 400.0,
        },
        "Mermaid".into(),
    );

    let (view, cx) = cx.add_window_view(|window, cx| {
        let trigger_focus = cx.focus_handle();
        window.focus(&trigger_focus, cx);
        let cached_island = cx.new(|_| CachedWorkspaceIsland {
            renders: Rc::new(Cell::new(0)),
        });
        PreviewE2eHarness {
            preview: None,
            generation: 0,
            trigger_focus,
            notifications: Vec::new(),
            items: vec![diagram_item],
            activity_image_urls: Vec::new(),
            cached_island,
        }
    });
    pump_frames(cx, 2);

    let open_trigger = cx.debug_bounds("e2e-open-trigger").expect("open trigger");
    cx.simulate_click(open_trigger.center(), Modifiers::none());
    pump_frames(cx, 4);

    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().expect("diagram preview open").read(cx);
        assert_eq!(modal.oriented_dimensions(), Some((1200, 800)));
        assert!(matches!(modal.zoom_mode(), ImagePreviewZoomMode::Fit));
    });

    // Zoom to 1:1 (600x400 logical exceeds 560x420 test window width) and drag to pan.
    cx.simulate_keystrokes("ctrl-1");
    pump_frames(cx, 3);
    let stage_bounds = cx
        .debug_bounds("image-preview-stage")
        .expect("visible preview stage");
    let drag_start = stage_bounds.center();
    let drag_end = gpui::point(drag_start.x - px(40.0), drag_start.y);
    cx.simulate_mouse_down(drag_start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(drag_end, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(drag_end, MouseButton::Left, Modifiers::none());
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().unwrap().read(cx);
        assert!(modal.scroll_offset().x < px(-1.0));
    });

    // Rotate Mermaid diagram 90 deg and copy as PNG.
    cx.simulate_keystrokes("r");
    pump_frames(cx, 6);
    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().unwrap().read(cx);
        assert_eq!(modal.current_rotation(), 1);
        assert_eq!(modal.oriented_dimensions(), Some((800, 1200)));
    });

    cx.simulate_keystrokes("ctrl-c");
    pump_frames(cx, 6);
    let clipboard = cx
        .read_from_clipboard()
        .expect("clipboard should hold Mermaid PNG");
    assert!(
        clipboard
            .entries()
            .iter()
            .any(|entry| matches!(entry, ClipboardEntry::Image(img) if !img.bytes.is_empty())),
        "expected PNG clipboard entry for Mermaid diagram"
    );

    cx.simulate_keystrokes("escape");
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, _| assert!(harness.preview.is_none()));
}

#[gpui::test]
fn image_preview_unavailable_image_stays_safe_e2e(cx: &mut TestAppContext) {
    init_test_app(cx);
    let items = vec![ImagePreviewItem::from_image(
        Arc::new(gpui::Image::empty()),
        "missing.png".into(),
        None,
        false,
    )];
    let (view, cx) = cx.add_window_view(|window, cx| {
        let trigger_focus = cx.focus_handle();
        window.focus(&trigger_focus, cx);
        let cached_island = cx.new(|_| CachedWorkspaceIsland {
            renders: Rc::new(Cell::new(0)),
        });
        PreviewE2eHarness {
            preview: None,
            generation: 0,
            trigger_focus,
            notifications: Vec::new(),
            items,
            activity_image_urls: Vec::new(),
            cached_island,
        }
    });
    pump_frames(cx, 2);

    let open_trigger = cx.debug_bounds("e2e-open-trigger").expect("open trigger");
    cx.simulate_click(open_trigger.center(), Modifiers::none());
    pump_frames(cx, 4);

    // Zoom / rotate / copy on an empty image must stay no-op and never panic.
    cx.simulate_keystrokes("ctrl-1 r ctrl-c");
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, cx| {
        let modal = harness.preview.as_ref().expect("preview still open").read(cx);
        assert_eq!(modal.current_rotation(), 0);
        assert!(matches!(modal.zoom_mode(), ImagePreviewZoomMode::Fit));
        assert!(harness.notifications.is_empty());
    });

    cx.simulate_keystrokes("escape");
    pump_frames(cx, 2);
    cx.read_entity(&view, |harness, _| assert!(harness.preview.is_none()));
}
