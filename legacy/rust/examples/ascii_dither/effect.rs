use std::sync::{Arc, Mutex, OnceLock};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use image::{DynamicImage, Rgba, RgbaImage, imageops::FilterType};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Filter {
    Ascii,
    Dither,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    Shadows,
    Highlights,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Palette {
    Original,
    Meadow,
    Blue,
    Amber,
    Lavender,
    Mono,
}

impl Palette {
    pub const ALL: [Self; 6] = [
        Self::Original,
        Self::Meadow,
        Self::Blue,
        Self::Amber,
        Self::Lavender,
        Self::Mono,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Original => "原色",
            Self::Meadow => "草甸",
            Self::Blue => "海蓝",
            Self::Amber => "琥珀",
            Self::Lavender => "薰衣草",
            Self::Mono => "黑白",
        }
    }

    pub fn stops(self) -> Option<[[u8; 3]; 4]> {
        Some(match self {
            Self::Original => return None,
            Self::Meadow => [
                [43, 54, 31],
                [103, 125, 69],
                [174, 186, 120],
                [229, 225, 189],
            ],
            Self::Blue => [
                [25, 42, 77],
                [50, 106, 197],
                [162, 194, 241],
                [237, 243, 252],
            ],
            Self::Amber => [[60, 32, 15], [167, 76, 21], [239, 154, 70], [247, 231, 202]],
            Self::Lavender => [
                [41, 33, 68],
                [101, 82, 171],
                [179, 165, 228],
                [240, 236, 251],
            ],
            Self::Mono => [[21, 21, 21], [87, 87, 87], [166, 166, 166], [236, 236, 236]],
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Settings {
    pub mask: u8,
    pub palette: Palette,
    pub tone: Tone,
    pub strength: u8,
    pub size: u8,
    pub spacing: u8,
}

impl Settings {
    pub fn recommended(filter: Filter) -> Self {
        Self {
            mask: if filter == Filter::Ascii { 90 } else { 88 },
            palette: Palette::Original,
            tone: Tone::Shadows,
            strength: 100,
            size: if filter == Filter::Ascii { 37 } else { 15 },
            spacing: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderSpec {
    pub width: u32,
    pub height: u32,
    pub scale_milli: u16,
    pub dark: bool,
}

impl RenderSpec {
    fn scale(self) -> f32 {
        f32::from(self.scale_milli) / 1000.0
    }
}

// Accessed only by the renderer's single background worker, never during a frame.
fn source(width: u32, height: u32) -> Arc<RgbaImage> {
    static SOURCE: OnceLock<DynamicImage> = OnceLock::new();
    static RESIZED: Mutex<Option<(u32, u32, Arc<RgbaImage>)>> = Mutex::new(None);
    let mut cached = RESIZED.lock().expect("image cache poisoned");
    if let Some((w, h, image)) = &*cached
        && *w == width
        && *h == height
    {
        return image.clone();
    }
    let source = SOURCE.get_or_init(|| {
        image::load_from_memory(include_bytes!("landscape.png"))
            .expect("embedded demo image must be valid")
    });
    let image = Arc::new(
        source
            .resize_to_fill(width, height, FilterType::Triangle)
            .to_rgba8(),
    );
    *cached = Some((width, height, image.clone()));
    image
}

fn luminance(pixel: &[u8]) -> f32 {
    (0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32) / 255.0
}

fn color_map(stops: [[u8; 3]; 4]) -> [[u8; 3]; 256] {
    let mut map = [[0; 3]; 256];
    let positions = [0.0, 0.4, 0.74, 1.0];
    for (i, color) in map.iter_mut().enumerate() {
        let l = i as f32 / 255.0;
        let segment = if l < 0.4 {
            0
        } else if l < 0.74 {
            1
        } else {
            2
        };
        let t = (l - positions[segment]) / (positions[segment + 1] - positions[segment]);
        for (c, value) in color.iter_mut().enumerate() {
            *value = (stops[segment][c] as f32 * (1.0 - t) + stops[segment + 1][c] as f32 * t)
                .round() as u8;
        }
    }
    map
}

fn mapped_source(settings: Settings, spec: RenderSpec) -> RgbaImage {
    let mut image = (*source(spec.width, spec.height)).clone();
    if let Some(stops) = settings.palette.stops() {
        let map = color_map(stops);
        for pixel in image.pixels_mut() {
            let rgb = map[(luminance(&pixel.0) * 255.0).round() as usize];
            pixel.0[..3].copy_from_slice(&rgb);
        }
    }
    image
}

fn mask(image: &mut RgbaImage, amount: u8, dark: bool) {
    let bases = if dark {
        [0.44, 0.20, 0.50]
    } else {
        [0.54, 0.20, 0.38]
    };
    let v = f32::from(amount);
    let opacity = bases.map(|base: f32| {
        if v <= 50.0 {
            base * (v / 50.0).powf(1.08)
        } else {
            base + (1.0 - base) * ((v - 50.0) / 50.0).powf(1.08)
        }
    });
    let height = image.height().max(2) as f32 - 1.0;
    let target = if dark { 0.0 } else { 255.0 };
    for (y, row) in image.rows_mut().enumerate() {
        let p = y as f32 / height;
        let (a, b, t) = if p <= 0.42 {
            (opacity[0], opacity[1], p / 0.42)
        } else {
            (opacity[1], opacity[2], (p - 0.42) / 0.58)
        };
        let alpha = a + (b - a) * t;
        for pixel in row {
            for c in &mut pixel.0[..3] {
                *c = (*c as f32 * (1.0 - alpha) + target * alpha).round() as u8;
            }
        }
    }
}

fn percentiles(image: &RgbaImage) -> (f32, f32) {
    let stride = ((image.width() as usize * image.height() as usize) / 12_000).max(1);
    let mut hist = [0u32; 256];
    let mut total = 0u32;
    for pixel in image.pixels().step_by(stride) {
        hist[(luminance(&pixel.0) * 255.0).round() as usize] += 1;
        total += 1;
    }
    let mut count = 0;
    let mut low = 0;
    let mut high = 255;
    for (index, n) in hist.into_iter().enumerate() {
        count += n;
        if count < total * 8 / 100 {
            low = index;
        }
        if count >= total * 92 / 100 {
            high = index;
            break;
        }
    }
    (low as f32 / 255.0, high as f32 / 255.0)
}

struct GlyphMask {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    pixels: Vec<f32>,
}

fn ascii(image: &mut RgbaImage, sample: &RgbaImage, settings: Settings, spec: RenderSpec) {
    const CHARS: &[u8] = b"  .,:;-=+xX08S#&@";
    let font = FontRef::try_from_slice(include_bytes!(
        "../../assets/fonts/JetBrainsMono-Regular.ttf"
    ))
    .expect("embedded font must be valid");
    let scale = spec.scale();
    let logical_width = spec.width as f32 / scale;
    let max_font = if logical_width < 720.0 { 22.0 } else { 24.0 };
    let font_size = (7.0 + (max_font - 7.0) * f32::from(settings.size) / 100.0).round() * scale;
    let font = font.as_scaled(PxScale::from(font_size));
    let advance = font.h_advance(font.glyph_id('@'));
    let line_height = font.height();
    let step_x = ((8.0 + 16.0 * f32::from(settings.spacing) / 100.0) * scale).max(advance * 1.08);
    let step_y = ((9.0 + 17.0 * f32::from(settings.spacing) / 100.0) * scale)
        .max(line_height * 1.12)
        .max(font_size * 1.04);
    let glyphs: Vec<Option<GlyphMask>> = CHARS
        .iter()
        .map(|ch| {
            let mut glyph = font.scaled_glyph(*ch as char);
            glyph.position = point(0.0, font.ascent());
            font.outline_glyph(glyph).map(|outline| {
                let bounds = outline.px_bounds();
                let width = bounds.width() as u32;
                let height = bounds.height() as u32;
                let mut pixels = vec![0.0; (width * height) as usize];
                outline.draw(|x, y, coverage| pixels[(y * width + x) as usize] = coverage);
                GlyphMask {
                    x: bounds.min.x as i32,
                    y: bounds.min.y as i32,
                    width,
                    height,
                    pixels,
                }
            })
        })
        .collect();
    let (low, high) = percentiles(sample);
    let strength = f32::from(settings.strength) / 100.0;
    let dark_ink = settings.tone == Tone::Shadows;
    let rows = (spec.height as f32 / step_y).ceil() as usize;
    let cols = (spec.width as f32 / step_x).ceil() as usize;
    for row in 0..rows {
        for col in 0..cols {
            let cell_x = col as f32 * step_x;
            let cell_y = row as f32 * step_y;
            let x = (cell_x + step_x * 0.5).min((spec.width - 1) as f32) as u32;
            let y = (cell_y + step_y * 0.5).min((spec.height - 1) as f32) as u32;
            let pixel = sample.get_pixel(x, y).0;
            let l = luminance(&pixel);
            let raw = if dark_ink { 1.0 - l } else { l };
            let normalized = if high - low < 0.025 {
                raw
            } else if dark_ink {
                ((high - l) / (high - low)).clamp(0.0, 1.0)
            } else {
                ((l - low) / (high - low)).clamp(0.0, 1.0)
            };
            let mix = 0.72 + 0.14 * strength;
            let t = raw * (1.0 - mix) + normalized * mix;
            let black = 0.22 * strength;
            let white = 1.0 - 0.08 * strength;
            let level = ((t - black) / (white - black)).clamp(0.0, 1.0);
            let smooth = level * level * (3.0 - 2.0 * level);
            let tone = level * (1.0 - strength) + smooth * strength;
            if tone <= 0.02 {
                continue;
            }
            let index = (tone * 16.0).round() as usize;
            let Some(glyph) = &glyphs[index] else {
                continue;
            };
            let rgb = lifted(pixel, tone, strength);
            let alpha = ((0.12 + 0.7 * tone) * (0.72 + 0.28 * strength)).clamp(0.06, 0.94);
            let left = (cell_x + (step_x - advance) * 0.5).round() as i32 + glyph.x;
            let top = (cell_y + (step_y - line_height) * 0.5).round() as i32 + glyph.y;
            for gy in 0..glyph.height {
                for gx in 0..glyph.width {
                    let x = left + gx as i32;
                    let y = top + gy as i32;
                    if x < 0 || y < 0 || x >= spec.width as i32 || y >= spec.height as i32 {
                        continue;
                    }
                    let opacity = glyph.pixels[(gy * glyph.width + gx) as usize] * alpha;
                    if opacity == 0.0 {
                        continue;
                    }
                    let dest = image.get_pixel_mut(x as u32, y as u32);
                    for (c, color) in rgb.iter().enumerate() {
                        dest.0[c] =
                            (dest.0[c] as f32 * (1.0 - opacity) + color * opacity).round() as u8;
                    }
                }
            }
        }
    }
}

fn lifted(pixel: [u8; 4], tone: f32, strength: f32) -> [f32; 3] {
    let rgb = [
        pixel[0] as f32 / 255.0,
        pixel[1] as f32 / 255.0,
        pixel[2] as f32 / 255.0,
    ];
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let value = max + (1.0 - max) * (0.24 + 0.18 * strength + 0.24 * tone);
    let saturation = if max == 0.0 {
        0.0
    } else {
        ((max - min) / max * (1.16 + 0.2 * strength)).min(1.0)
    };
    rgb.map(|channel| {
        let hue = if max == min {
            0.0
        } else {
            (channel - min) / (max - min)
        };
        value * (1.0 - saturation + saturation * hue) * 255.0
    })
}

fn dither(image: &mut RgbaImage, sample: &RgbaImage, settings: Settings, spec: RenderSpec) {
    const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let strength = f32::from(settings.strength) / 100.0;
    let max_size = if spec.width as f32 / spec.scale() < 720.0 {
        7.0
    } else {
        9.0
    };
    let block = ((2.0 + (max_size - 2.0) * f32::from(settings.size) / 100.0).round() * spec.scale())
        .round()
        .max(2.0) as u32;
    let pattern = (1.0 + 3.0 * f32::from(settings.spacing) / 100.0)
        .round()
        .max(1.0) as u32;
    let levels = (8.0 - 6.0 * strength).round().max(2.0);
    let blend = 0.32 + 0.68 * strength;
    for by in 0..spec.height.div_ceil(block) {
        for bx in 0..spec.width.div_ceil(block) {
            let x = (bx * block + block / 2).min(spec.width - 1);
            let y = (by * block + block / 2).min(spec.height - 1);
            let sample = sample.get_pixel(x, y).0;
            let l = luminance(&sample) * 255.0;
            let threshold =
                (f32::from(BAYER[(by / pattern % 4) as usize][(bx / pattern % 4) as usize]) + 0.5)
                    / 16.0;
            let mut color = [0; 4];
            color[3] = 255;
            for c in 0..3 {
                let original =
                    (l + (sample[c] as f32 - l) * (1.06 + 0.18 * strength)).clamp(0.0, 255.0);
                let scaled = original / 255.0 * (levels - 1.0);
                let quantized = (scaled.floor() + f32::from(scaled.fract() > threshold))
                    / (levels - 1.0)
                    * 255.0;
                color[c] = (original + (quantized - original) * blend)
                    .clamp(0.0, 255.0)
                    .round() as u8;
            }
            for y in by * block..((by + 1) * block).min(spec.height) {
                for x in bx * block..((bx + 1) * block).min(spec.width) {
                    image.put_pixel(x, y, Rgba(color));
                }
            }
        }
    }
}

pub fn render(filter: Filter, settings: Settings, spec: RenderSpec) -> RgbaImage {
    let sample = mapped_source(settings, spec);
    let mut image = sample.clone();
    match filter {
        Filter::Ascii => {
            mask(&mut image, settings.mask, spec.dark);
            ascii(&mut image, &sample, settings, spec);
        }
        Filter::Dither => {
            dither(&mut image, &sample, settings, spec);
            mask(&mut image, settings.mask, spec.dark);
        }
    }
    image
}
