//! Headless GPUI E2E for loading local Markdown images in session history.
//! Exercises real PNG files on disk, relative/percent-encoded/absolute/`file:`
//! resolution, virtualized `ListState` rows, and async background asset loads.

use std::io::Cursor;
use std::path::PathBuf;

use gpui::{Context, ListAlignment, ListState, Render, TestAppContext, list, prelude::*, px};

use super::*;

struct Harness {
    markdown: MarkdownView,
    selection: TranscriptSelection,
    workspace: PathBuf,
    list: ListState,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        div().w(px(480.0)).h(px(1400.0)).child(
            list(self.list.clone(), move |_, _, cx| {
                view.read_with(cx, |this, _| {
                    let palette = Palette::from_theme(&Theme::dark());
                    let ctx = Ctx::new(
                        "local-image-e2e",
                        &palette,
                        Metrics::body(),
                        this.selection.clone(),
                    )
                    .with_workspace_path(Some(this.workspace.as_path()));
                    div()
                        .w_full()
                        .min_w_0()
                        .child(frame_reset(this.selection.clone()))
                        .children(markdown(&this.markdown, &ctx))
                        .into_any_element()
                })
            })
            .size_full(),
        )
    }
}

fn write_png(path: &std::path::Path, width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent directories");
    }
    let buffer = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
    let mut bytes = Vec::new();
    buffer
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .expect("encode PNG");
    std::fs::write(path, &bytes).expect("write PNG file");
    bytes
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
fn session_history_loads_local_markdown_images_e2e(cx: &mut TestAppContext) {
    // Failure modes: relative or percent-encoded local paths fall back to GPUI's
    // embedded asset loader; `file://` URLs and absolute paths hit NullHttpClient;
    // tall local images exceed MAX_HEIGHT; missing files crash layout or drop
    // their alt caption; virtualized list rows stay stuck at pre-load height.
    let root =
        std::env::temp_dir().join(format!("fintwind-local-image-e2e-{}", uuid::Uuid::new_v4()));
    let workspace = root.join("workspace");
    let external = root.join("external");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir_all(&external).expect("create external");

    write_png(&workspace.join("cat.png"), 120, 80, [240, 150, 40, 255]);
    write_png(
        &workspace.join("sub dir").join("orange cat.png"),
        60,
        40,
        [235, 130, 30, 255],
    );
    let abs_path = external.join("abs.png");
    write_png(&abs_path, 160, 100, [80, 170, 220, 255]);
    let file_url_path = external.join("via file url.png");
    write_png(&file_url_path, 200, 120, [90, 200, 120, 255]);
    let tall_path = workspace.join("tall.png");
    write_png(&tall_path, 100, 640, [180, 90, 210, 255]);
    let data_bytes = write_png(&external.join("data-src.png"), 48, 32, [200, 60, 80, 255]);

    use base64::Engine as _;
    let data_url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&data_bytes)
    );
    let file_url = url::Url::from_file_path(&file_url_path).expect("file URL");
    let abs_display = abs_path.to_string_lossy().replace('\\', "/");

    let source = format!(
        "![相对路径](cat.png)\n\n\
         ![编码路径](sub%20dir/orange%20cat.png)\n\n\
         ![绝对路径]({abs_display})\n\n\
         ![文件URL]({file_url})\n\n\
         ![过高限制](tall.png)\n\n\
         ![内联数据]({data_url})\n\n\
         ![缺失图注](missing.png)\n\n\
         ![远端链接](https://example.com/remote.png)\n"
    );

    let (_view, cx) = cx.add_window_view(|_, _| {
        let mut markdown = MarkdownView::new();
        markdown.set_text(&source, false);
        let list = ListState::new(1, ListAlignment::Top, px(48.0));
        Harness {
            markdown,
            selection: TranscriptSelection::default(),
            workspace: workspace.clone(),
            list,
        }
    });

    pump_frames(cx, 6);

    let selector = |block_ix: usize| -> &'static str {
        Box::leak(format!("image-local-image-e2e-{}", block_ix << 16).into_boxed_str())
    };
    let relative_bounds = cx
        .debug_bounds(selector(0))
        .expect("relative local image bounds");
    let encoded_bounds = cx
        .debug_bounds(selector(1))
        .expect("percent-encoded local image bounds");
    let abs_bounds = cx
        .debug_bounds(selector(2))
        .expect("absolute local image bounds");
    let file_url_bounds = cx
        .debug_bounds(selector(3))
        .expect("file:// local image bounds");
    let tall_bounds = cx
        .debug_bounds(selector(4))
        .expect("tall local image bounds");
    let data_bounds = cx.debug_bounds(selector(5)).expect("data URL image bounds");
    let missing_bounds = cx
        .debug_bounds(selector(6))
        .expect("missing local image bounds");
    let remote_bounds = cx
        .debug_bounds(selector(7))
        .expect("remote URL image bounds");

    assert_eq!(relative_bounds.size.height, px(80.0));
    assert_eq!(encoded_bounds.size.height, px(40.0));
    assert_eq!(abs_bounds.size.height, px(100.0));
    assert_eq!(file_url_bounds.size.height, px(120.0));
    assert!(
        tall_bounds.size.height > px(0.0) && tall_bounds.size.height <= px(320.0),
        "tall image height {:?} must clamp to MAX_HEIGHT (320px)",
        tall_bounds.size.height
    );
    assert_eq!(data_bounds.size.height, px(32.0));
    assert_eq!(missing_bounds.size.height, px(0.0));
    assert_eq!(remote_bounds.size.height, px(0.0));

    let _ = std::fs::remove_dir_all(&root);
}
