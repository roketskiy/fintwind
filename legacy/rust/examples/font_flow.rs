//! Standalone GPUI text-shimmer demo: `cargo run --example font_flow`.

use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Bounds, Context, FontWeight, IntoElement, ParentElement, Render, Styled,
    StyledText, TextRun, WindowBounds, WindowOptions, div, font, point, px, rgb, size,
};

const LABEL: &str = "Churning";
const CYCLE: f32 = 2.4;

struct FontFlow {
    started: Instant,
}

impl FontFlow {
    fn new(cx: &mut Context<Self>) -> Self {
        // A single ticker drives both the shimmer and the elapsed counter. The
        // window is tiny, so 30 fps is enough for smooth movement without a
        // display-rate animation loop. Dropping the view ends the task.
        cx.spawn(async move |this, cx| {
            loop {
                let interval = cx.update(|cx| if cx.reduce_motion() { 100 } else { 33 });
                cx.background_executor()
                    .timer(Duration::from_millis(interval))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            started: Instant::now(),
        }
    }
}

// A soft band crosses the actual glyphs. GPUI text runs keep this a single
// shaped text element, rather than moving eight separate letters in layout.
fn flowing_label(phase: f32) -> StyledText {
    let font = font("Segoe UI");
    let runs = LABEL
        .char_indices()
        .map(|(start, ch)| {
            let position = start as f32 / (LABEL.len() - 1) as f32;
            let center = phase * 1.6 - 0.3;
            let distance = ((position - center) / 0.28).abs();
            let glow = (1.0 - distance).max(0.0);
            let glow = glow * glow * (3.0 - 2.0 * glow);
            let base = [105.0, 118.0, 135.0];
            let highlight = [16.0, 28.0, 46.0];
            let channel = |n: usize| (base[n] + (highlight[n] - base[n]) * glow) as u32;
            TextRun {
                len: ch.len_utf8(),
                font: font.clone(),
                color: rgb((channel(0) << 16) | (channel(1) << 8) | channel(2)).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }
        })
        .collect();
    StyledText::new(LABEL).with_runs(runs)
}

impl Render for FontFlow {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let elapsed = self.started.elapsed().as_secs_f32();
        let phase = if cx.reduce_motion() {
            // The counter keeps working when system animations are disabled.
            -1.0
        } else {
            (elapsed / CYCLE).fract()
        };

        div()
            .size_full()
            .bg(rgb(0xf8fafc))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .h(px(60.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(13.0))
                    .text_size(px(16.0))
                    .font_family("Segoe UI")
                    .font_weight(FontWeight::NORMAL)
                    .child(flowing_label(phase))
                    .child(
                        div()
                            .ml(px(1.0))
                            .text_color(rgb(0x626b78))
                            .child(format!("{elapsed:.1}s")),
                    ),
            )
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        // GPUI does not read this Windows preference itself. Mirror the main
        // application's startup behavior for this independent example.
        #[cfg(target_os = "windows")]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
            };
            let mut enabled: i32 = 1;
            let read = unsafe {
                SystemParametersInfoW(
                    SPI_GETCLIENTAREAANIMATION,
                    0,
                    std::ptr::from_mut(&mut enabled).cast(),
                    0,
                )
            };
            if read != 0 {
                cx.set_reduce_motion(enabled == 0);
            }
        }
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(250.0), px(250.0)),
                    size(px(330.0), px(130.0)),
                ))),
                ..Default::default()
            },
            |_, cx| cx.new(FontFlow::new),
        )
        .expect("failed to open font flow window");
    });
}
