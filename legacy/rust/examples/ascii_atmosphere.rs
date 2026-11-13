//! Animated ASCII background: `cargo run --manifest-path examples/ascii_atmosphere_standalone/Cargo.toml`.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Bounds, Context, FocusHandle, InteractiveElement, IntoElement, KeyBinding,
    KeyDownEvent, ParentElement, Render, RenderImage, Role, StatefulInteractiveElement, Styled,
    Window, WindowBounds, WindowOptions, actions, div, img, px, relative, rgb, size,
};
use image::{Frame, Rgba, RgbaImage};

const FIELD_HEIGHT: f32 = 0.55;
const CELL_SIZE: f32 = 13.0;
const CHARACTERS: &[u8] = b" =+xX0S8#@";

actions!(ascii_atmosphere, [NextFocus, PreviousFocus]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    const ALL: [Self; 2] = [Self::Light, Self::Dark];

    fn name(self) -> &'static str {
        match self {
            Self::Light => "白底",
            Self::Dark => "深底",
        }
    }

    fn background(self) -> [u8; 4] {
        match self {
            Self::Light => [255, 255, 255, 255],
            Self::Dark => [18, 15, 23, 255],
        }
    }
}

#[derive(Clone, Copy)]
struct Palette {
    name: &'static str,
    swatch: u32,
    colors: [[f32; 3]; 4],
}

const PALETTES: [Palette; 5] = [
    Palette {
        name: "深蓝",
        swatch: 0x3159ed,
        colors: [
            [30.0, 63.0, 216.0],
            [246.0, 248.0, 255.0],
            [165.0, 216.0, 255.0],
            [56.0, 189.0, 248.0],
        ],
    },
    Palette {
        name: "紫色",
        swatch: 0x9d6bf0,
        colors: [
            [100.0, 54.0, 205.0],
            [249.0, 243.0, 255.0],
            [215.0, 180.0, 255.0],
            [153.0, 108.0, 246.0],
        ],
    },
    Palette {
        name: "琥珀",
        swatch: 0xf49138,
        colors: [
            [205.0, 76.0, 23.0],
            [255.0, 246.0, 228.0],
            [255.0, 190.0, 113.0],
            [248.0, 146.0, 44.0],
        ],
    },
    Palette {
        name: "青绿",
        swatch: 0x32caa4,
        colors: [
            [6.0, 135.0, 102.0],
            [235.0, 255.0, 246.0],
            [158.0, 238.0, 208.0],
            [56.0, 212.0, 171.0],
        ],
    },
    Palette {
        name: "玫红",
        swatch: 0xf1669c,
        colors: [
            [194.0, 36.0, 91.0],
            [255.0, 241.0, 247.0],
            [255.0, 172.0, 205.0],
            [247.0, 95.0, 162.0],
        ],
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FieldSize {
    width: u32,
    height: u32,
    scale_milli: u16,
}

impl FieldSize {
    fn scale(self) -> f32 {
        f32::from(self.scale_milli) / 1000.0
    }
}

fn smoothstep(start: f32, end: f32, value: f32) -> f32 {
    let t = ((value - start) / (end - start)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn noise(x: u32, y: u32) -> f32 {
    let mut n = x.wrapping_mul(0x9e37_79b9) ^ y.wrapping_mul(0x85eb_ca6b);
    n ^= n >> 16;
    n = n.wrapping_mul(0x7feb_352d);
    n ^= n >> 15;
    n = n.wrapping_mul(0x846c_a68b);
    n ^= n >> 16;
    (n & 0xffff) as f32 / 65535.0
}

struct Glyph {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    coverage: Vec<f32>,
}

fn glyphs(scale: f32) -> (Vec<Option<Glyph>>, f32, f32) {
    let font = FontRef::try_from_slice(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"))
        .expect("embedded font must be valid");
    let font = font.as_scaled(PxScale::from(CELL_SIZE * 0.92 * scale));
    let advance = font.h_advance(font.glyph_id('@'));
    let height = font.height();
    let glyphs = CHARACTERS
        .iter()
        .map(|&ch| {
            let mut glyph = font.scaled_glyph(ch as char);
            glyph.position = point(0.0, font.ascent());
            font.outline_glyph(glyph).map(|outline| {
                let bounds = outline.px_bounds();
                let width = bounds.width() as u32;
                let height = bounds.height() as u32;
                let mut coverage = vec![0.0; (width * height) as usize];
                outline.draw(|x, y, value| coverage[(y * width + x) as usize] = value);
                Glyph {
                    x: bounds.min.x as i32,
                    y: bounds.min.y as i32,
                    width,
                    height,
                    coverage,
                }
            })
        })
        .collect();
    (glyphs, advance, height)
}

// The Rise field and palette are adapted from Gradient ASCII by Bruno Delorence.
// See ascii_atmosphere.MIT-LICENSE for the original copyright and license.
fn field_hash(x: f32, y: f32) -> f32 {
    let mut x = x * 123.34;
    let mut y = y * 456.21;
    x -= x.floor();
    y -= y.floor();
    let dot = x * (x + 45.32) + y * (y + 45.32);
    x += dot;
    y += dot;
    let product = x * y;
    product - product.floor()
}

fn field_noise(x: f32, y: f32) -> f32 {
    let ix = x.floor();
    let iy = y.floor();
    let fx = smoothstep(0.0, 1.0, x - ix);
    let fy = smoothstep(0.0, 1.0, y - iy);
    let top = field_hash(ix, iy) * (1.0 - fx) + field_hash(ix + 1.0, iy) * fx;
    let bottom = field_hash(ix, iy + 1.0) * (1.0 - fx) + field_hash(ix + 1.0, iy + 1.0) * fx;
    top * (1.0 - fy) + bottom * fy
}

#[derive(Clone, Copy)]
struct Light {
    strength: f32,
    center: f32,
    sky: f32,
    white: f32,
    color: [f32; 3],
}

fn rise_light(x: f32, y: f32, size: FieldSize, seconds: f32, palette: &Palette) -> Light {
    let height = size.height as f32 / FIELD_HEIGHT;
    let mut px = (x - size.width as f32 * 0.5) / height;
    let mut py = (height - size.height as f32 + y - height * 0.5) / height;
    let warp_x = field_noise(px * 1.7 + seconds * 0.13, py * 1.7 - seconds * 0.08) - 0.5;
    let warp_y = field_noise(px * 1.7 - seconds * 0.1 + 3.7, py * 1.7 + seconds * 0.12) - 0.5;
    px += warp_x * 0.065;
    py += warp_y * 0.065;
    py += (px * 2.6 + seconds * 0.5).sin() * 0.015;

    let radius = ((px + 0.085) * 0.385).hypot(py - 0.678);
    let depth = 0.62 - radius;
    let strength = smoothstep(0.043, 0.219, depth);
    let mix_to_sky = smoothstep(0.198, 0.318, depth);
    let center = 1.0 - smoothstep(0.129, 0.307, radius);
    let [base, white, pale, accent] = palette.colors;
    let position = center * 2.0;
    let (from, to, amount) = if position < 1.0 {
        (white, pale, position)
    } else {
        (pale, accent, position - 1.0)
    };
    let reference_red = if position < 1.0 {
        246.0 * (1.0 - position) + 165.0 * position
    } else {
        165.0 * (2.0 - position) + 56.0 * (position - 1.0)
    };
    let mut color = [0.0; 3];
    for channel in 0..3 {
        let sky = from[channel] * (1.0 - amount) + to[channel] * amount;
        color[channel] = base[channel] * (1.0 - mix_to_sky) + sky * mix_to_sky;
    }
    Light {
        strength,
        center,
        sky: mix_to_sky,
        white: (mix_to_sky * (reference_red - 30.0) / 216.0).clamp(0.0, 1.0),
        color,
    }
}

// The frame is prepared off the UI thread; only its finished image is painted.
fn render_field(
    size: FieldSize,
    seconds: f32,
    palette_index: usize,
    appearance: Appearance,
) -> RgbaImage {
    let palette = &PALETTES[palette_index];
    let background = appearance.background();
    let scale = size.scale();
    let sample_step = (2.0 * scale).max(1.0);
    let sample_width = (size.width as f32 / sample_step).ceil() as u32;
    let sample_height = (size.height as f32 / sample_step).ceil() as u32;
    let mut samples = RgbaImage::new(sample_width, sample_height);
    for (y, row) in samples.rows_mut().enumerate() {
        for (x, pixel) in row.enumerate() {
            let sx = (x as f32 + 0.5) * size.width as f32 / sample_width as f32;
            let sy = (y as f32 + 0.5) * size.height as f32 / sample_height as f32;
            let light = rise_light(sx, sy, size, seconds, palette);
            let alpha = light.strength * 0.49;
            for channel in 0..3 {
                let wash = match appearance {
                    Appearance::Light => light.color[channel] * 0.88 + 255.0 * 0.12,
                    Appearance::Dark => light.color[channel],
                };
                pixel.0[channel] =
                    (background[channel] as f32 * (1.0 - alpha) + wash * alpha).round() as u8;
            }
            pixel.0[3] = 255;
        }
    }
    let mut image = image::imageops::resize(
        &samples,
        size.width,
        size.height,
        image::imageops::FilterType::Triangle,
    );
    let grain_phase = (seconds.max(0.0) * 7.13).floor() as u32;
    for (y, row) in image.rows_mut().enumerate() {
        for (x, pixel) in row.enumerate() {
            let light = (0..3)
                .map(|channel| match appearance {
                    Appearance::Light => background[channel] as f32 - pixel.0[channel] as f32,
                    Appearance::Dark => pixel.0[channel] as f32 - background[channel] as f32,
                })
                .fold(0.0_f32, f32::max)
                / 115.0;
            let grain = (noise(
                (x as u32).wrapping_add(grain_phase.wrapping_mul(91)),
                (y as u32).wrapping_add(grain_phase.wrapping_mul(37)),
            ) - 0.5)
                * 18.0
                * light.clamp(0.0, 1.0);
            for channel in 0..3 {
                pixel.0[channel] = (pixel.0[channel] as f32 + grain).clamp(0.0, 255.0) as u8;
            }
        }
    }

    let full_height = size.height as f32 / FIELD_HEIGHT;
    let field_top = full_height - size.height as f32;
    let step_x = (CELL_SIZE * 0.6 * scale).round().max(1.0);
    let step_y = (CELL_SIZE * scale).round().max(1.0);
    let columns = (size.width as f32 / step_x).ceil() as u32;
    let rows = (full_height / step_y).ceil() as u32;
    let offset_x = ((size.width as f32 - columns as f32 * step_x) * 0.5).floor();
    let offset_y = ((full_height - rows as f32 * step_y) * 0.5).floor();
    let first_row = ((field_top - offset_y) / step_y).floor().max(0.0) as u32;
    let (glyphs, advance, font_height) = glyphs(scale);

    for row in first_row..rows {
        let top = offset_y + row as f32 * step_y - field_top;
        let glyph_y = top + step_y * 0.5;
        for col in 0..columns {
            let left = offset_x + col as f32 * step_x;
            let glyph_x = left + step_x * 0.5;
            let light = rise_light(glyph_x, glyph_y, size, seconds, palette);
            let brightness =
                light.strength.powf(0.6) * (0.35 + 0.38 * light.center + 0.55 * light.white);
            if brightness < 0.035 {
                continue;
            }
            if noise(col.wrapping_add(731), row.wrapping_add(503)) < 0.09 {
                continue;
            }
            let rate = 0.8 + 2.4 * noise(col.wrapping_add(19), row.wrapping_add(9));
            let tick = (seconds * rate + noise(col, row) * 7.0).floor() as u32;
            let swap = noise(
                col.wrapping_add(tick.wrapping_mul(3)),
                row.wrapping_add(tick.wrapping_mul(5)),
            );
            let jitter = if swap < 0.09 {
                if noise(col.wrapping_add(tick), row.wrapping_add(23)) < 0.5 {
                    -1.0
                } else {
                    1.0
                }
            } else {
                0.0
            };
            let variation = (noise(col.wrapping_add(71), row.wrapping_add(193)) - 0.5) * 1.5;
            let index = (brightness * (CHARACTERS.len() - 1) as f32 + jitter + variation)
                .round()
                .clamp(0.0, (CHARACTERS.len() - 1) as f32) as usize;
            let Some(glyph) = &glyphs[index] else {
                continue;
            };
            let phase =
                noise(col.wrapping_add(1001), row.wrapping_add(171)) * std::f32::consts::TAU;
            let speed = 1.5 + 2.1 * noise(col.wrapping_add(359), row.wrapping_add(872));
            let wave = 0.5 + 0.5 * (seconds * speed + phase).sin();
            let opacity = smoothstep(0.02, 0.55, light.strength) * (0.72 + 0.28 * wave);
            let ink = match appearance {
                Appearance::Light => light
                    .color
                    .map(|color| color * (0.62 - 0.20 * light.center * light.sky)),
                Appearance::Dark => light.color.map(|color| (color * 1.42 + 7.65).min(255.0)),
            };
            let left = (left + (step_x - advance) * 0.5).round() as i32 + glyph.x;
            let top = (top + (step_y - font_height) * 0.5).round() as i32 + glyph.y;
            let softened = noise(col.wrapping_add(97), row.wrapping_add(229)) < 0.18;
            let coverage = |gx: i32, gy: i32| {
                if gx < 0 || gy < 0 || gx >= glyph.width as i32 || gy >= glyph.height as i32 {
                    0.0
                } else {
                    glyph.coverage[(gy as u32 * glyph.width + gx as u32) as usize]
                }
            };
            for gy in -1..=glyph.height as i32 {
                for gx in -1..=glyph.width as i32 {
                    let x = left + gx;
                    let y = top + gy;
                    if x < 0 || y < 0 || x >= size.width as i32 || y >= size.height as i32 {
                        continue;
                    }
                    let crisp = coverage(gx, gy);
                    let blur = (coverage(gx - 1, gy)
                        + coverage(gx + 1, gy)
                        + coverage(gx, gy - 1)
                        + coverage(gx, gy + 1))
                        * 0.25;
                    let mask = if softened {
                        crisp * 0.55 + blur * 0.45
                    } else {
                        crisp
                    };
                    let alpha = ((mask + blur * 0.15 * (1.0 - mask)) * opacity).min(1.0);
                    if alpha == 0.0 {
                        continue;
                    }
                    let pixel = image.get_pixel_mut(x as u32, y as u32);
                    for (channel, color) in ink.iter().enumerate() {
                        pixel.0[channel] =
                            (pixel.0[channel] as f32 * (1.0 - alpha) + color * alpha).round() as u8;
                    }
                }
            }
        }
    }
    image
}

struct Atmosphere {
    size: Option<FieldSize>,
    image: Option<Arc<RenderImage>>,
    started: Instant,
    rendering: bool,
    active: bool,
    root_focus: FocusHandle,
    palette_focus: Vec<FocusHandle>,
    appearance_focus: Vec<FocusHandle>,
    palette: usize,
    appearance: Appearance,
}

impl Atmosphere {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root_focus = cx.focus_handle();
        window.focus(&root_focus, cx);
        let palette_focus = PALETTES
            .iter()
            .map(|_| cx.focus_handle().tab_index(0).tab_stop(true))
            .collect();
        let appearance_focus = Appearance::ALL
            .iter()
            .map(|_| cx.focus_handle().tab_index(0).tab_stop(true))
            .collect();
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if view
                    .update(cx, |this, cx| {
                        if this.active && !cx.reduce_motion() {
                            this.request_frame(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            size: None,
            image: None,
            started: Instant::now(),
            rendering: false,
            active: true,
            root_focus,
            palette_focus,
            appearance_focus,
            palette: 0,
            appearance: Appearance::Light,
        }
    }

    fn set_palette(&mut self, palette: usize, cx: &mut Context<Self>) {
        if self.palette != palette {
            self.palette = palette;
            self.request_frame(cx);
            cx.notify();
        }
    }

    fn set_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        if self.appearance != appearance {
            self.appearance = appearance;
            if let Some(previous) = self.image.take() {
                cx.defer(move |cx| cx.drop_image(previous, None));
            }
            self.request_frame(cx);
            cx.notify();
        }
    }

    fn request_frame(&mut self, cx: &mut Context<Self>) {
        let Some(size) = self.size else { return };
        if self.rendering {
            return;
        }
        self.rendering = true;
        let seconds = if cx.reduce_motion() {
            0.0
        } else {
            self.started.elapsed().as_secs_f32()
        };
        let palette = self.palette;
        let appearance = self.appearance;
        let task = cx.background_executor().spawn(async move {
            let mut bitmap = render_field(size, seconds, palette, appearance);
            for pixel in bitmap.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            Arc::new(RenderImage::new(vec![Frame::new(bitmap)]))
        });
        cx.spawn(async move |view, cx| {
            let image = task.await;
            let _ = view.update(cx, |this, cx| {
                this.rendering = false;
                if this.size == Some(size)
                    && this.palette == palette
                    && this.appearance == appearance
                {
                    if let Some(previous) = this.image.replace(image) {
                        cx.defer(move |cx| cx.drop_image(previous, None));
                    }
                    cx.notify();
                } else {
                    this.request_frame(cx);
                }
            });
        })
        .detach();
    }
}

impl Render for Atmosphere {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.active = window.is_window_active();
        let viewport = window.viewport_size();
        let width = f32::from(viewport.width).max(1.0);
        let height = f32::from(viewport.height).max(1.0);
        let scale = window.scale_factor().min(1.5).min(1800.0 / width).max(0.25);
        let size = FieldSize {
            width: ((width * scale / 4.0).round() * 4.0).max(4.0) as u32,
            height: ((height * FIELD_HEIGHT * scale / 4.0).round() * 4.0).max(4.0) as u32,
            scale_milli: (scale * 1000.0).round() as u16,
        };
        if self.size != Some(size) {
            self.size = Some(size);
            self.request_frame(cx);
        }
        let (
            panel,
            border,
            selected_background,
            selected_border,
            hover,
            text,
            secondary,
            focus_color,
        ) = match self.appearance {
            Appearance::Light => (
                0xf6f6f9, 0xd8d9e1, 0xffffff, 0xb6b9c6, 0xe9eaf0, 0x22232c, 0x515461, 0x355ce6,
            ),
            Appearance::Dark => (
                0x1b1922, 0x302d39, 0x302e39, 0x787386, 0x302e39, 0xf3f1f8, 0xb9b5c2, 0xe4e2ed,
            ),
        };
        let mut appearance_buttons = Vec::with_capacity(Appearance::ALL.len());
        for (index, appearance) in Appearance::ALL.iter().copied().enumerate() {
            let selected = self.appearance == appearance;
            let focus_handle = self.appearance_focus[index].clone();
            appearance_buttons.push(
                div()
                    .id(("appearance", index))
                    .track_focus(&self.appearance_focus[index])
                    .tab_index(0)
                    .tab_stop(true)
                    .role(Role::Button)
                    .aria_label(appearance.name())
                    .h(px(38.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(rgb(if selected { selected_border } else { panel }))
                    .bg(rgb(if selected { selected_background } else { panel }))
                    .text_size(px(12.0))
                    .text_color(rgb(if selected { text } else { secondary }))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(hover)).text_color(rgb(text)))
                    .focus_visible(move |style| style.border_color(rgb(focus_color)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&focus_handle, cx);
                        this.set_appearance(appearance, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        let next = match event.keystroke.key.as_str() {
                            "enter" | "space" => Some(index),
                            "left" | "right" => Some(1 - index),
                            "home" => Some(0),
                            "end" => Some(1),
                            _ => None,
                        };
                        if let Some(next) = next {
                            window.focus(&this.appearance_focus[next], cx);
                            this.set_appearance(Appearance::ALL[next], cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(appearance.name()),
            );
        }
        let mut buttons = Vec::with_capacity(PALETTES.len());
        for (index, palette) in PALETTES.iter().enumerate() {
            let selected = self.palette == index;
            let focus = self.palette_focus[index].clone();
            buttons.push(
                div()
                    .id(("palette", index))
                    .track_focus(&self.palette_focus[index])
                    .tab_index(0)
                    .tab_stop(true)
                    .role(Role::Button)
                    .aria_label(palette.name)
                    .h(px(38.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.0))
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(rgb(if selected { selected_border } else { panel }))
                    .bg(rgb(if selected { selected_background } else { panel }))
                    .text_size(px(12.0))
                    .text_color(rgb(if selected { text } else { secondary }))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(hover)).text_color(rgb(text)))
                    .focus_visible(move |style| style.border_color(rgb(focus_color)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&focus, cx);
                        this.set_palette(index, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        let next = match event.keystroke.key.as_str() {
                            "enter" | "space" => Some(index),
                            "left" => Some((index + PALETTES.len() - 1) % PALETTES.len()),
                            "right" => Some((index + 1) % PALETTES.len()),
                            "home" => Some(0),
                            "end" => Some(PALETTES.len() - 1),
                            _ => None,
                        };
                        if let Some(next) = next {
                            window.focus(&this.palette_focus[next], cx);
                            this.set_palette(next, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(div().size(px(9.0)).rounded_full().bg(rgb(palette.swatch)))
                    .child(palette.name),
            );
        }
        div()
            .id("atmosphere")
            .track_focus(&self.root_focus)
            .on_action(cx.listener(|_, _: &NextFocus, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &PreviousFocus, window, cx| window.focus_prev(cx)))
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(rgb(match self.appearance {
                Appearance::Light => 0xffffff,
                Appearance::Dark => 0x120f17,
            }))
            .when_some(self.image.clone(), |root, image| {
                root.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(relative(FIELD_HEIGHT))
                        .child(img(image).size_full()),
                )
            })
            .child(
                div()
                    .id("palette-controls")
                    .tab_group()
                    .absolute()
                    .top(px(16.0))
                    .right(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .p(px(4.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(rgb(border))
                    .bg(rgb(panel))
                    .children(appearance_buttons)
                    .child(div().w(px(1.0)).h(px(20.0)).mx(px(4.0)).bg(rgb(border)))
                    .children(buttons),
            )
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("--export")) {
        let path = std::path::PathBuf::from(args.next().expect("provide a PNG path"));
        let width = args
            .next()
            .and_then(|s| s.to_string_lossy().parse::<u32>().ok())
            .unwrap_or(1080)
            .clamp(1, 4096);
        let height = args
            .next()
            .and_then(|s| s.to_string_lossy().parse::<u32>().ok())
            .unwrap_or(600)
            .clamp(1, 4096);
        let seconds = args
            .next()
            .and_then(|s| s.to_string_lossy().parse::<f32>().ok())
            .filter(|value| value.is_finite())
            .unwrap_or(0.0);
        let palette = args
            .next()
            .and_then(|s| s.to_string_lossy().parse::<usize>().ok())
            .filter(|index| *index < PALETTES.len())
            .unwrap_or(0);
        let appearance = match args.next().as_deref() {
            Some(value) if value == std::ffi::OsStr::new("dark") => Appearance::Dark,
            _ => Appearance::Light,
        };
        let field_height = ((height as f32 * FIELD_HEIGHT).round() as u32).max(1);
        let field = render_field(
            FieldSize {
                width,
                height: field_height,
                scale_milli: 1000,
            },
            seconds,
            palette,
            appearance,
        );
        let mut background = RgbaImage::from_pixel(width, height, Rgba(appearance.background()));
        image::imageops::replace(&mut background, &field, 0, i64::from(height - field_height));
        background.save(&path).expect("save background PNG");
        println!("{}", path.display());
        return;
    }

    gpui_platform::application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("tab", NextFocus, None),
            KeyBinding::new("shift-tab", PreviousFocus, None),
        ]);
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
        let bounds = Bounds::centered(None, size(px(1080.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(620.0), px(420.0))),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Gradient ASCII · 白底");
                cx.new(|cx| Atmosphere::new(window, cx))
            },
        )
        .expect("open atmosphere window");
        cx.activate(true);
    });
}
