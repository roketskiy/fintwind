//! Standalone image effect demo: `cargo run --example ascii_dither`.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[path = "ascii_dither/effect.rs"]
mod effect;

use std::{cell::Cell, rc::Rc, sync::Arc};

use effect::{Filter, Palette, RenderSpec, Settings, Tone};
use gpui::{
    App, AppContext, Bounds, Context, Div, DragMoveEvent, EmptyView, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, Pixels, Render, RenderImage, Role, Stateful, StatefulInteractiveElement, Styled,
    Window, WindowAppearance, WindowBounds, WindowOptions, actions, canvas, div, img,
    linear_color_stop, linear_gradient, prelude::*, px, relative, rgb, size,
};
use image::Frame;

actions!(ascii_demo, [NextFocus, PreviousFocus]);

#[derive(Clone, Copy)]
struct Colors {
    background: u32,
    panel: u32,
    field: u32,
    raised: u32,
    border: u32,
    text: u32,
    secondary: u32,
    accent: u32,
}

impl Colors {
    fn new(dark: bool) -> Self {
        if dark {
            Self {
                background: 0x101416,
                panel: 0x191e20,
                field: 0x252c2e,
                raised: 0x353f40,
                border: 0x343d3e,
                text: 0xf0f3ed,
                secondary: 0xa7b2aa,
                accent: 0xa4c6ab,
            }
        } else {
            Self {
                background: 0xf1f3ef,
                panel: 0xffffff,
                field: 0xe8ece8,
                raised: 0xffffff,
                border: 0xd9e0da,
                text: 0x202823,
                secondary: 0x67746b,
                accent: 0x4a7656,
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Command {
    Filter(Filter),
    Palette(Palette),
    Tone(Tone),
    Reset,
    Theme,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Slider {
    Mask,
    Strength,
    Size,
    Spacing,
}

impl Slider {
    fn name(self) -> &'static str {
        match self {
            Self::Mask => "遮罩",
            Self::Strength => "采样强度",
            Self::Size => "尺寸",
            Self::Spacing => "间距",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Mask => "mask",
            Self::Strength => "strength",
            Self::Size => "size",
            Self::Spacing => "spacing",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Mask => 10,
            Self::Strength => 11,
            Self::Size => 12,
            Self::Spacing => 13,
        }
    }

    fn value(self, settings: Settings) -> u8 {
        match self {
            Self::Mask => settings.mask,
            Self::Strength => settings.strength,
            Self::Size => settings.size,
            Self::Spacing => settings.spacing,
        }
    }

    fn set(self, settings: &mut Settings, value: u8) {
        match self {
            Self::Mask => settings.mask = value,
            Self::Strength => settings.strength = value,
            Self::Size => settings.size = value,
            Self::Spacing => settings.spacing = value,
        }
    }
}

struct SliderDrag(Slider);

struct Demo {
    root: FocusHandle,
    focus: Vec<FocusHandle>,
    filter: Filter,
    settings: [Settings; 2],
    dark: bool,
    spec: Option<RenderSpec>,
    generation: u64,
    rendering: bool,
    image: Option<Arc<RenderImage>>,
}

impl Demo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = cx.focus_handle();
        window.focus(&root, cx);
        window.set_window_title("ASCII / Dither Demo");
        Self {
            root,
            focus: (0..16)
                .map(|_| cx.focus_handle().tab_index(0).tab_stop(true))
                .collect(),
            filter: Filter::Ascii,
            settings: [
                Settings::recommended(Filter::Ascii),
                Settings::recommended(Filter::Dither),
            ],
            dark: matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
            spec: None,
            generation: 0,
            rendering: false,
            image: None,
        }
    }

    fn current_index(&self) -> usize {
        if self.filter == Filter::Ascii { 0 } else { 1 }
    }

    fn settings(&self) -> Settings {
        self.settings[self.current_index()]
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.start_render(cx);
        cx.notify();
    }

    fn start_render(&mut self, cx: &mut Context<Self>) {
        let Some(spec) = self.spec else { return };
        if self.rendering {
            return;
        }
        self.rendering = true;
        let filter = self.filter;
        let settings = self.settings();
        let generation = self.generation;
        let task = cx.background_executor().spawn(async move {
            let mut bitmap = effect::render(filter, settings, spec);
            // GPUI's image atlas expects BGRA.
            for pixel in bitmap.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            Arc::new(RenderImage::new(vec![Frame::new(bitmap)]))
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.rendering = false;
                if this.filter == filter
                    && this.settings().palette == settings.palette
                    && this.dark == spec.dark
                {
                    if let Some(previous) = this.image.replace(result) {
                        cx.drop_image(previous, None);
                    }
                    cx.notify();
                }
                if this.generation != generation {
                    this.start_render(cx);
                }
            });
        })
        .detach();
    }

    fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        match command {
            Command::Filter(filter) => self.filter = filter,
            Command::Palette(palette) => {
                let index = self.current_index();
                self.settings[index].palette = palette;
            }
            Command::Tone(tone) => {
                let index = self.current_index();
                self.settings[index].tone = tone;
            }
            Command::Reset => {
                let index = self.current_index();
                self.settings[index] = Settings::recommended(self.filter);
            }
            Command::Theme => {
                self.dark = !self.dark;
                if let Some(spec) = &mut self.spec {
                    spec.dark = self.dark;
                }
            }
        }
        self.changed(cx);
    }

    fn set_slider(&mut self, slider: Slider, value: u8, cx: &mut Context<Self>) {
        let index = self.current_index();
        if slider.value(self.settings[index]) == value {
            return;
        }
        slider.set(&mut self.settings[index], value);
        self.changed(cx);
    }

    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        index: usize,
        command: Command,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let c = Colors::new(self.dark);
        let focus = self.focus[index].clone();
        div()
            .id(id)
            .track_focus(&self.focus[index])
            .tab_index(0)
            .tab_stop(true)
            .role(Role::Button)
            .aria_label(label)
            .h(px(32.0))
            .px(px(13.0))
            .rounded(px(7.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.0))
            .font_weight(if selected {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .bg(rgb(if selected { c.raised } else { c.field }))
            .text_color(rgb(if selected { c.text } else { c.secondary }))
            .hover(move |s| s.bg(rgb(c.raised)).text_color(rgb(c.text)))
            .focus_visible(move |s| s.border_1().border_color(rgb(c.accent)))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&focus, cx);
                this.command(command, cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                let next = match (command, key) {
                    (_, "enter" | "space") => Some(command),
                    (Command::Filter(_), "left" | "right") => {
                        Some(Command::Filter(if this.filter == Filter::Ascii {
                            Filter::Dither
                        } else {
                            Filter::Ascii
                        }))
                    }
                    (Command::Tone(_), "left" | "right") => {
                        Some(Command::Tone(if this.settings().tone == Tone::Shadows {
                            Tone::Highlights
                        } else {
                            Tone::Shadows
                        }))
                    }
                    _ => None,
                };
                if let Some(next) = next {
                    this.command(next, cx);
                    cx.stop_propagation();
                }
            }))
            .child(label)
    }

    fn swatch(&self, palette: Palette, index: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let c = Colors::new(self.dark);
        let selected = self.settings().palette == palette;
        let stops = palette.stops().unwrap_or([
            [29, 43, 45],
            [82, 125, 104],
            [219, 170, 94],
            [236, 236, 236],
        ]);
        let to_hex =
            |rgb: [u8; 3]| (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
        let focus = self.focus[index + 2].clone();
        div()
            .id(("palette", index))
            .track_focus(&self.focus[index + 2])
            .tab_index(0)
            .tab_stop(true)
            .role(Role::Button)
            .aria_label(palette.name())
            .w(px(32.0))
            .h(px(36.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.0))
            .focus_visible(move |s| s.border_1().border_color(rgb(c.accent)))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&focus, cx);
                this.command(Command::Palette(palette), cx);
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let next_index = match key {
                    "enter" | "space" => Some(index),
                    "left" => Some((index + Palette::ALL.len() - 1) % Palette::ALL.len()),
                    "right" => Some((index + 1) % Palette::ALL.len()),
                    "home" => Some(0),
                    "end" => Some(Palette::ALL.len() - 1),
                    _ => None,
                };
                if let Some(next) = next_index {
                    window.focus(&this.focus[next + 2], cx);
                    this.command(Command::Palette(Palette::ALL[next]), cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .border_color(rgb(if selected { c.accent } else { c.panel }))
                    .child(
                        div()
                            .w(px(22.0))
                            .h(px(22.0))
                            .rounded_full()
                            .bg(linear_gradient(
                                135.0,
                                linear_color_stop(rgb(to_hex(stops[1])), 0.0),
                                linear_color_stop(rgb(to_hex(stops[2])), 1.0),
                            )),
                    ),
            )
    }

    fn slider(&self, slider: Slider, cx: &mut Context<Self>) -> Stateful<Div> {
        let c = Colors::new(self.dark);
        let value = slider.value(self.settings());
        let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
        let measured = bounds.clone();
        let focus = self.focus[slider.index()].clone();
        div()
            .id(slider.id())
            .track_focus(&self.focus[slider.index()])
            .tab_index(0)
            .tab_stop(true)
            .role(Role::Slider)
            .aria_label(slider.name())
            .aria_numeric_value(f64::from(value))
            .aria_min_numeric_value(0.0)
            .aria_max_numeric_value(100.0)
            .aria_numeric_value_step(1.0)
            .h(px(34.0))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(10.0))
            .focus_visible(move |s| s.border_1().border_color(rgb(c.accent)).rounded(px(7.0)))
            .child(
                div()
                    .id((slider.id(), 1usize))
                    .relative()
                    .flex_1()
                    .h(px(30.0))
                    .child(
                        div()
                            .absolute()
                            .top(px(4.0))
                            .left_0()
                            .right_0()
                            .h(px(22.0))
                            .rounded(px(8.0))
                            .bg(rgb(c.field)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(4.0))
                            .left_0()
                            .h(px(22.0))
                            .w(relative(value as f32 / 100.0))
                            .rounded(px(8.0))
                            .bg(rgb(c.accent)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(relative(0.5))
                            .top(px(10.0))
                            .h(px(10.0))
                            .w(px(1.0))
                            .bg(rgb(c.secondary)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(relative(value as f32 / 100.0))
                            .ml(px(-4.5))
                            .top_0()
                            .w(px(9.0))
                            .h(px(30.0))
                            .rounded(px(3.0))
                            .bg(rgb(c.raised))
                            .border_1()
                            .border_color(rgb(c.border)),
                    )
                    .child(
                        canvas(move |rect, _, _| measured.set(rect), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let rect = bounds.get();
                            if rect.size.width > px(0.0) {
                                let fraction = (event.position.x - rect.left()) / rect.size.width;
                                this.set_slider(
                                    slider,
                                    (fraction.clamp(0.0, 1.0) * 100.0).round() as u8,
                                    cx,
                                );
                            }
                            window.focus(&focus, cx);
                        }),
                    )
                    .on_drag(SliderDrag(slider), |_, _, _, cx| cx.new(|_| EmptyView))
                    .on_drag_move(cx.listener(
                        move |this, event: &DragMoveEvent<SliderDrag>, _, cx| {
                            if event.drag(cx).0 != slider {
                                return;
                            }
                            let fraction = (event.event.position.x - event.bounds.left())
                                / event.bounds.size.width;
                            this.set_slider(
                                slider,
                                (fraction.clamp(0.0, 1.0) * 100.0).round() as u8,
                                cx,
                            );
                        },
                    )),
            )
            .child(
                div()
                    .w(px(42.0))
                    .h(px(30.0))
                    .rounded(px(6.0))
                    .bg(rgb(c.field))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(rgb(c.text))
                    .child(format!("{value}%")),
            )
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                let value = i16::from(slider.value(this.settings()));
                let next = match event.keystroke.key.as_str() {
                    "left" | "down" => value - 1,
                    "right" | "up" => value + 1,
                    "pagedown" => value - 10,
                    "pageup" => value + 10,
                    "home" => 0,
                    "end" => 100,
                    _ => return,
                };
                this.set_slider(slider, next.clamp(0, 100) as u8, cx);
                cx.stop_propagation();
            }))
    }

    fn row(&self, label: &'static str, control: impl IntoElement) -> Div {
        let c = Colors::new(self.dark);
        div()
            .w_full()
            .h(px(48.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .w(px(88.0))
                    .flex_none()
                    .text_size(px(13.0))
                    .text_color(rgb(c.secondary))
                    .child(label),
            )
            .child(control)
    }
}

impl Render for Demo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::new(self.dark);
        let viewport = window.viewport_size();
        let available_width = (f32::from(viewport.width) - 64.0 - 24.0 - 380.0).max(260.0);
        let available_height = (f32::from(viewport.height) - 150.0).max(200.0);
        let preview_width = available_width.min(available_height * 1.5);
        let preview_height = preview_width / 1.5;
        let dpi = window.scale_factor().clamp(1.0, 1.6);
        let logical_width = (preview_width / 4.0).round() * 4.0;
        let spec = RenderSpec {
            width: (logical_width * dpi).round() as u32,
            height: (logical_width / 1.5 * dpi).round() as u32,
            scale_milli: (dpi * 1000.0).round() as u16,
            dark: self.dark,
        };
        if self.spec != Some(spec) {
            self.spec = Some(spec);
            self.changed(cx);
        }

        let mode = div()
            .flex()
            .gap(px(4.0))
            .child(self.button(
                "ascii",
                "ASCII",
                0,
                Command::Filter(Filter::Ascii),
                self.filter == Filter::Ascii,
                cx,
            ))
            .child(self.button(
                "dither",
                "Dither",
                1,
                Command::Filter(Filter::Dither),
                self.filter == Filter::Dither,
                cx,
            ));
        let palette = div()
            .flex()
            .items_center()
            .children(
                Palette::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| self.swatch(p, i, cx))
                    .collect::<Vec<_>>(),
            )
            .child(
                div()
                    .ml(px(4.0))
                    .text_size(px(11.0))
                    .text_color(rgb(c.secondary))
                    .child(self.settings().palette.name()),
            );
        let tone = div()
            .flex()
            .gap(px(4.0))
            .child(self.button(
                "shadows",
                "阴影",
                8,
                Command::Tone(Tone::Shadows),
                self.settings().tone == Tone::Shadows,
                cx,
            ))
            .child(self.button(
                "highlights",
                "高光",
                9,
                Command::Tone(Tone::Highlights),
                self.settings().tone == Tone::Highlights,
                cx,
            ));

        div()
            .id("demo")
            .track_focus(&self.root)
            .on_action(cx.listener(|_, _: &NextFocus, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &PreviousFocus, window, cx| window.focus_prev(cx)))
            .size_full()
            .bg(rgb(c.background))
            .text_color(rgb(c.text))
            .font_family("Segoe UI")
            .flex()
            .flex_col()
            .px(px(32.0))
            .pb(px(30.0))
            .child(
                div()
                    .w_full()
                    .h(px(86.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .text_size(px(21.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("字符与抖动"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(c.secondary))
                                    .child("ASCII / DITHER"),
                            ),
                    )
                    .child(self.button(
                        "theme",
                        if self.dark { "浅色" } else { "深色" },
                        15,
                        Command::Theme,
                        false,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(24.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(
                                div()
                                    .w(px(preview_width))
                                    .h(px(preview_height))
                                    .rounded(px(14.0))
                                    .overflow_hidden()
                                    .border_1()
                                    .border_color(rgb(c.border))
                                    .bg(rgb(c.field))
                                    .when_some(self.image.clone(), |el, image| {
                                        el.child(img(image).size_full())
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .w(px(380.0))
                            .flex_none()
                            .rounded(px(14.0))
                            .border_1()
                            .border_color(rgb(c.border))
                            .bg(rgb(c.panel))
                            .px(px(22.0))
                            .py(px(18.0))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .h(px(46.0))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(16.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("效果设置"),
                                    )
                                    .child(self.button(
                                        "reset",
                                        "恢复推荐",
                                        14,
                                        Command::Reset,
                                        false,
                                        cx,
                                    )),
                            )
                            .child(self.row("遮罩", self.slider(Slider::Mask, cx)))
                            .child(self.row("滤镜", mode))
                            .child(self.row("配色", palette))
                            .when(self.filter == Filter::Ascii, |panel| {
                                panel.child(self.row("采样色调", tone))
                            })
                            .child(self.row("采样强度", self.slider(Slider::Strength, cx)))
                            .child(self.row("尺寸", self.slider(Slider::Size, cx)))
                            .child(self.row("间距", self.slider(Slider::Spacing, cx))),
                    ),
            )
    }
}

fn main() {
    if let Some(dir) = std::env::args_os()
        .nth(1)
        .filter(|arg| arg == "--render-samples")
    {
        let _ = dir;
        let destination = std::env::args_os()
            .nth(2)
            .expect("provide an output directory");
        let destination = std::path::PathBuf::from(destination);
        std::fs::create_dir_all(&destination).expect("create output directory");
        let spec = RenderSpec {
            width: 900,
            height: 600,
            scale_milli: 1000,
            dark: true,
        };
        for (name, filter) in [("ascii.png", Filter::Ascii), ("dither.png", Filter::Dither)] {
            let path = destination.join(name);
            effect::render(filter, Settings::recommended(filter), spec)
                .save(&path)
                .expect("save sample");
            println!("{}", path.display());
        }
        return;
    }
    gpui_platform::application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("tab", NextFocus, None),
            KeyBinding::new("shift-tab", PreviousFocus, None),
        ]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.0), px(760.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(980.0), px(640.0))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Demo::new(window, cx)),
        )
        .expect("open demo window");
        cx.activate(true);
    });
}
