//! Animated ASCII light for an unsent session.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use gpui::prelude::FluentBuilder;
use gpui::{
    Bounds, Context, Hsla, IntoElement, ParentElement, Pixels, Render, RenderImage, Rgba, Styled,
    Window, canvas, div, img, relative,
};
use image::{Frame, RgbaImage};

use super::NEW_SESSION_MOVE;
use crate::theme::Theme;

const FIELD_HEIGHT: f32 = 0.55;
const CELL_SIZE: f32 = 13.0;
const CHARACTERS: &[u8] = b" =+xX0S8#@";
const FRAME_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy)]
enum Appearance {
    Light,
    Dark,
}

struct Palette {
    colors: [[f32; 3]; 4],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ThemeColors {
    background: [u8; 4],
    stops: [[u8; 3]; 4],
    is_dark: bool,
}

fn color_bytes(color: Hsla) -> [u8; 3] {
    let color: Rgba = color.into();
    [color.r, color.g, color.b].map(|value| (value * 255.0).round() as u8)
}

impl ThemeColors {
    fn from_theme(theme: Theme) -> Self {
        let background_rgb = color_bytes(theme.surface);
        let mut base = theme.accent;
        base.l = if theme.is_dark { 0.46 } else { 0.44 };
        base.s = (base.s * 1.1).min(0.9);
        let mut inner = base;
        inner.h = (inner.h - 0.045).rem_euclid(1.0);
        inner.l = if theme.is_dark { 0.65 } else { 0.60 };
        let base = color_bytes(base);
        let inner = color_bytes(inner);
        let white = [246, 248, 255];
        let mut pale = [0; 3];
        for channel in 0..3 {
            pale[channel] =
                (f32::from(white[channel]) * 0.55 + f32::from(inner[channel]) * 0.45).round() as u8;
        }
        Self {
            background: [background_rgb[0], background_rgb[1], background_rgb[2], 255],
            stops: [base, white, pale, inner],
            is_dark: theme.is_dark,
        }
    }
}

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
    let font = FontRef::try_from_slice(include_bytes!(
        "../../assets/fonts/JetBrainsMono-Regular.ttf"
    ))
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
// See licenses/gradient-ascii-MIT.txt for the original copyright and license.
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
fn render_field(size: FieldSize, seconds: f32, colors: ThemeColors) -> RgbaImage {
    let palette = Palette {
        colors: colors.stops.map(|stop| stop.map(f32::from)),
    };
    let palette = &palette;
    let background = colors.background;
    let appearance = if colors.is_dark {
        Appearance::Dark
    } else {
        Appearance::Light
    };
    let scale = size.scale();
    let sample_step = (4.0 * scale).max(1.0);
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
                    Appearance::Light => {
                        light.color[channel] * 0.88 + background[channel] as f32 * 0.12
                    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameSpec {
    size: FieldSize,
    colors: ThemeColors,
}

impl FrameSpec {
    fn for_bounds(bounds: Bounds<Pixels>, window: &Window, colors: ThemeColors) -> Option<Self> {
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        if width < 1.0 || height < 1.0 {
            return None;
        }
        // Keep work proportional to the visible page, bounded on large displays.
        let scale = window.scale_factor().min(1.0).min(1200.0 / width).max(0.25);
        Some(Self {
            size: FieldSize {
                width: ((width * scale / 4.0).round() * 4.0).max(4.0) as u32,
                height: ((height * FIELD_HEIGHT * scale / 4.0).round() * 4.0).max(4.0) as u32,
                scale_milli: (scale * 1000.0).round() as u16,
            },
            colors,
        })
    }
}

/// Retains the latest completed frame. Rasterization stays off the UI thread.
pub(super) struct NewSessionBackground {
    spec: Option<FrameSpec>,
    spec_generation: u64,
    image: Option<Arc<RenderImage>>,
    previous_image: Option<Arc<RenderImage>>,
    pending_image: Option<Arc<RenderImage>>,
    image_completed: Option<Instant>,
    blend_duration: Duration,
    started: Instant,
    last_started: Option<Instant>,
    fading_since: Option<Instant>,
    rendering: bool,
}

impl NewSessionBackground {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
        Self {
            spec: None,
            spec_generation: 0,
            image: None,
            previous_image: None,
            pending_image: None,
            image_completed: None,
            blend_duration: FRAME_INTERVAL,
            started: Instant::now(),
            last_started: None,
            fading_since: None,
            rendering: false,
        }
    }

    pub(super) fn fade_out(&mut self, started: Instant, cx: &mut Context<Self>) {
        self.fading_since = Some(started);
        self.pending_image = None;
        cx.notify();
    }

    pub(super) fn cancel_fade(&mut self, cx: &mut Context<Self>) {
        if self.fading_since.take().is_some() {
            cx.notify();
        }
    }

    fn set_spec(&mut self, spec: FrameSpec, cx: &mut Context<Self>) {
        if self.spec == Some(spec) {
            if self.image.is_none() && !self.rendering {
                self.request_frame(true, cx);
            }
            return;
        }
        self.spec = Some(spec);
        self.spec_generation = self.spec_generation.wrapping_add(1);
        self.pending_image = None;
        for previous in [self.image.take(), self.previous_image.take()]
            .into_iter()
            .flatten()
        {
            cx.defer(move |cx| cx.drop_image(previous, None));
        }
        self.image_completed = None;
        self.request_frame(true, cx);
    }

    fn request_frame(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(spec) = self.spec else { return };
        if self.rendering
            || self.pending_image.is_some()
            || (!force
                && self
                    .last_started
                    .is_some_and(|started| started.elapsed() < FRAME_INTERVAL))
        {
            return;
        }
        self.rendering = true;
        self.last_started = Some(Instant::now());
        let generation = self.spec_generation;
        let seconds = if cx.reduce_motion() {
            0.0
        } else {
            self.started.elapsed().as_secs_f32()
        };
        let task = cx.background_executor().spawn(async move {
            let mut bitmap = render_field(spec.size, seconds, spec.colors);
            // The GPUI image atlas uses BGRA.
            for pixel in bitmap.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            Arc::new(RenderImage::new(vec![Frame::new(bitmap)]))
        });
        cx.spawn(async move |view, cx| {
            let image = task.await;
            let _ = view.update(cx, |this, cx| {
                this.rendering = false;
                let preserve_fading_frame = this.image.is_some()
                    && this
                        .fading_since
                        .is_some_and(|started| started.elapsed() < NEW_SESSION_MOVE);
                if this.spec_generation == generation && !preserve_fading_frame {
                    let blending = !cx.reduce_motion()
                        && this.previous_image.is_some()
                        && this
                            .image_completed
                            .is_some_and(|completed| completed.elapsed() < this.blend_duration);
                    if blending {
                        this.pending_image = Some(image);
                    } else {
                        this.accept_image(image, Instant::now(), cx);
                    }
                }
                // A mounted view rechecks its bounds and requests the latest
                // spec, even when reduced motion leaves animation idle.
                cx.notify();
            });
        })
        .detach();
    }

    fn accept_image(&mut self, image: Arc<RenderImage>, now: Instant, cx: &mut Context<Self>) {
        self.blend_duration = self
            .image_completed
            .map(|completed| now.duration_since(completed))
            .unwrap_or(FRAME_INTERVAL)
            .clamp(FRAME_INTERVAL, FRAME_INTERVAL * 2);
        self.image_completed = Some(now);
        if let Some(previous) = self.image.replace(image)
            && let Some(retired) = self.previous_image.replace(previous)
        {
            cx.defer(move |cx| cx.drop_image(retired, None));
        }
    }
}

impl Render for NewSessionBackground {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ThemeColors::from_theme(Theme::current(cx));
        let active = window.is_window_active() && !cx.reduce_motion();
        let fading = self
            .fading_since
            .is_some_and(|started| started.elapsed() < NEW_SESSION_MOVE);
        if active {
            window.request_animation_frame();
        }
        let mut blend = if cx.reduce_motion() {
            1.0
        } else {
            self.image_completed.map_or(1.0, |completed| {
                let now = self.fading_since.unwrap_or_else(Instant::now);
                (now.saturating_duration_since(completed).as_secs_f32()
                    / self.blend_duration.as_secs_f32())
                .clamp(0.0, 1.0)
            })
        };
        if blend >= 1.0
            && !fading
            && let Some(pending) = self.pending_image.take()
        {
            self.accept_image(pending, Instant::now(), cx);
            blend = if cx.reduce_motion() { 1.0 } else { 0.0 };
        }
        if blend >= 1.0
            && let Some(previous) = self.previous_image.take()
        {
            cx.defer(move |cx| cx.drop_image(previous, None));
        }
        if active && !fading && self.spec.is_some_and(|spec| spec.colors == colors) {
            self.request_frame(false, cx);
        }
        let image = self
            .spec
            .filter(|spec| spec.colors == colors)
            .and(self.image.clone());
        let previous_image = self
            .spec
            .filter(|spec| spec.colors == colors)
            .and(self.previous_image.clone());
        let background = cx.entity().downgrade();
        div()
            .relative()
            .size_full()
            .child(
                canvas(
                    move |bounds, window, cx| {
                        if let Some(spec) = FrameSpec::for_bounds(bounds, window, colors) {
                            let _ = background.update(cx, |background, cx| {
                                background.set_spec(spec, cx);
                            });
                        }
                    },
                    |_, _, _, _| (),
                )
                .absolute()
                .inset_0(),
            )
            .when_some(image, |element, image| {
                let has_previous = previous_image.is_some();
                element
                    .when_some(previous_image, |element, previous| {
                        element.child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .w_full()
                                .h(relative(FIELD_HEIGHT))
                                .child(img(previous).size_full()),
                        )
                    })
                    .child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .w_full()
                            .h(relative(FIELD_HEIGHT))
                            .opacity(if has_previous { blend } else { 1.0 })
                            .child(img(image).size_full()),
                    )
            })
    }
}
