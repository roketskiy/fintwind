//! Display-math rendering: `$$…$$` → typeset PNG, on a background thread.
//!
//! The pipeline is the RaTeX family (a KaTeX port in pure Rust — no JS, no
//! WebView): `ratex_parser` turns LaTeX into an AST, `ratex_layout` typesets it
//! into a `DisplayList`, and `ratex_render` rasterizes that into PNG bytes with
//! the KaTeX fonts embedded in the binary. Nothing here touches the UI thread:
//! the render path only reads the cache below and paints whatever is already
//! there, degrading to the raw LaTeX source until (or unless) the raster
//! arrives.
//!
//! The cache is content-addressed — `(latex, font size, quantized paint
//! color)` — so a theme switch or a metrics change naturally produces a new
//! key and re-renders. Entries are immutable once stored, which also means a
//! re-render raced into by an evicted task writes byte-identical output; no
//! generation guard is needed. The store itself is a process-wide static
//! rather than a GPUI global because the *read* happens in the render path,
//! which has no `App` handle; the write path uses `AsyncApp::update` only to
//! reach `cx.refresh()`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, LazyLock};

use gpui::{App, ImageFormat};
use parking_lot::Mutex;

/// Cache bound in formulas, not bytes: a typical rendered equation is a few
/// KB of PNG, so 256 entries stay well under a megabyte while covering a
/// whole long conversation of math-heavy output.
const MAX_ENTRIES: usize = 256;
/// Formulas rasterize at 2× and display at logical size, so they stay crisp
/// on high-refresh HIDPI screens without per-scale-factor re-renders.
const DEVICE_PIXEL_RATIO: f32 = 2.0;
/// Transparent margin so glyph edges never touch the bitmap's bounds.
const PADDING: f32 = 3.0;

/// Content-addressed identity of one rendered formula.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct MathKey {
    latex: Arc<str>,
    /// Rounded body text size the formula is typeset at. Exact `f32` equality
    /// would fragment the cache across renderings of the same metrics.
    font_size: u32,
    /// The RGBA8 the raster actually paints with. Quantizing into the key lets
    /// two `Hsla`s that land on the same bytes share one entry.
    color: [u8; 4],
}

impl MathKey {
    fn new(latex: &str, font_size: f32, color: gpui::Hsla) -> Self {
        Self {
            latex: Arc::from(latex),
            font_size: font_size.round().max(1.0) as u32,
            color: quantize(color),
        }
    }
}

fn quantize(color: gpui::Hsla) -> [u8; 4] {
    let rgba = gpui::Rgba::from(color);
    [
        (rgba.r * 255.0) as u8,
        (rgba.g * 255.0) as u8,
        (rgba.b * 255.0) as u8,
        (rgba.a * 255.0).round() as u8,
    ]
}

enum MathImage {
    Ready {
        image: Arc<gpui::Image>,
        /// Logical size to lay the bitmap out at (pixels ÷ dpr).
        width: f32,
        height: f32,
    },
    Failed,
}

#[derive(Default)]
struct MathCacheState {
    entries: HashMap<MathKey, MathImage>,
    /// Renders currently on the background executor, deduplicating the
    /// per-frame kick so one formula renders exactly once.
    inflight: HashSet<MathKey>,
    /// Insertion order for FIFO eviction.
    order: VecDeque<MathKey>,
}

static CACHE: LazyLock<Mutex<MathCacheState>> =
    LazyLock::new(|| Mutex::new(MathCacheState::default()));

/// Inline `$…$` has no block geometry to preserve, so it renders as Unicode
/// text (`x^2 + y^2` → `x² + y²`) inside the surrounding run. Unknown input
/// degrades to itself; the conversion is best-effort by design.
pub fn to_unicode(latex: &str) -> String {
    let text = unicodeit::replace(latex);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        latex.trim().to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The cached raster for `key`, if one has already been rendered. Render-path
/// only: a miss means "not ready yet", and the caller shows the fallback.
pub fn cached(
    latex: &str,
    font_size: f32,
    color: gpui::Hsla,
) -> Option<(Arc<gpui::Image>, f32, f32)> {
    let key = MathKey::new(latex, font_size, color);
    let cache = CACHE.lock();
    match cache.entries.get(&key) {
        Some(MathImage::Ready {
            image,
            width,
            height,
        }) => Some((image.clone(), *width, *height)),
        Some(MathImage::Failed) | None => None,
    }
}

/// Kick a background render for `key` unless one is cached or already running.
/// Called from a paint pass, which has the `App` handle the render path lacks.
/// Idempotent per frame by construction: cache hit and inflight mark both
/// return without touching the executor.
pub fn ensure_rendered(latex: &str, font_size: f32, color: gpui::Hsla, cx: &mut App) {
    let key = MathKey::new(latex, font_size, color);
    {
        let mut cache = CACHE.lock();
        match cache.entries.get(&key) {
            Some(MathImage::Ready { .. } | MathImage::Failed) => return,
            None => {}
        }
        if !cache.inflight.insert(key.clone()) {
            return;
        }
    }

    cx.spawn(async move |cx| {
        let task_key = key.clone();
        let rendered = cx
            .background_executor()
            .spawn(async move { render_formula(&task_key) })
            .await;
        let image = match rendered {
            Ok((image, width, height)) => MathImage::Ready {
                image,
                width,
                height,
            },
            Err(_) => MathImage::Failed,
        };
        cx.update(|_| {
            let mut cache = CACHE.lock();
            cache.inflight.remove(&key);
            insert_and_evict(&mut cache, key, image);
        });
        cx.refresh();
    })
    .detach();
}

fn insert_and_evict(cache: &mut MathCacheState, key: MathKey, image: MathImage) {
    // Only first inserts extend the eviction order; overwriting an existing
    // entry (Ready raced against an identical re-render) keeps its position.
    if cache.entries.insert(key.clone(), image).is_none() {
        cache.order.push_back(key);
    }
    while cache.entries.len() > MAX_ENTRIES {
        let Some(oldest) = cache.order.pop_front() else {
            break;
        };
        cache.entries.remove(&oldest);
    }
}

/// Parse → typeset → rasterize, entirely off the UI thread. Returns the PNG
/// plus its logical layout size.
fn render_formula(key: &MathKey) -> Result<(Arc<gpui::Image>, f32, f32), String> {
    let nodes = ratex_parser::parse(&key.latex).map_err(|error| format!("{error:?}"))?;
    let [r, g, b, a] = key.color.map(f32::from).map(|channel| channel / 255.0);
    let layout_box = ratex_layout::layout(
        &nodes,
        &ratex_layout::LayoutOptions {
            style: ratex_types::MathStyle::Display,
            color: ratex_types::Color { r, g, b, a },
            ..Default::default()
        },
    );
    let display_list = ratex_layout::to_display_list(&layout_box);
    let options = ratex_render::RenderOptions {
        font_size: key.font_size as f32,
        padding: PADDING,
        background_color: ratex_types::Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        },
        font_dir: String::new(),
        device_pixel_ratio: DEVICE_PIXEL_RATIO,
    };
    let png = ratex_render::render_to_png(&display_list, &options)?;

    // Mirror render_to_png's own pixel arithmetic to recover the logical size
    // the bitmap lays out at, instead of decoding the PNG header.
    let em_px = key.font_size as f32 * DEVICE_PIXEL_RATIO;
    let pad_px = PADDING * DEVICE_PIXEL_RATIO;
    let width = ((display_list.width as f32 * em_px + 2.0 * pad_px).ceil() as u32).max(1);
    let height = (((display_list.height + display_list.depth) as f32 * em_px + 2.0 * pad_px).ceil()
        as u32)
        .max(1);

    Ok((
        Arc::new(gpui::Image::from_bytes(ImageFormat::Png, png)),
        width as f32 / DEVICE_PIXEL_RATIO,
        height as f32 / DEVICE_PIXEL_RATIO,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_conversion_maps_commands_and_keeps_unknown_text() {
        assert_eq!(to_unicode(r"x^2 + y^2"), "x² + y²");
        assert_eq!(to_unicode(r"\alpha \beta"), "α β");
        // Not math at all: degrades to itself rather than emptying out.
        assert_eq!(to_unicode("plain text"), "plain text");
    }

    #[test]
    fn quantized_colors_share_one_cache_identity() {
        let base = gpui::hsla(0.0, 0.0, 0.9, 1.0);
        let key = MathKey::new(r"\int", 14.0, base);
        // Same rounded size and color → same key, regardless of f32 noise.
        let same = MathKey::new(r"\int", 14.2, gpui::hsla(0.0, 0.0, 0.9005, 1.0));
        assert_eq!(key, same);
        assert_ne!(key, MathKey::new(r"\int", 15.0, base));
        assert_ne!(key, MathKey::new(r"\sum", 14.0, base));
    }

    #[test]
    fn cache_evicts_oldest_beyond_the_bound() {
        let mut cache = MathCacheState::default();
        let entry = |font_size: u32| {
            (
                MathKey {
                    latex: Arc::from("latex"),
                    font_size,
                    color: [0; 4],
                },
                MathImage::Ready {
                    image: Arc::new(gpui::Image::from_bytes(ImageFormat::Png, Vec::new())),
                    width: 10.0,
                    height: 10.0,
                },
            )
        };
        for index in 0..(MAX_ENTRIES + 8) {
            let (key, image) = entry(index as u32);
            insert_and_evict(&mut cache, key, image);
        }
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert_eq!(cache.order.len(), MAX_ENTRIES);
        // The oldest eight were evicted; the newest survive.
        let oldest = MathKey {
            latex: Arc::from("latex"),
            font_size: 0,
            color: [0; 4],
        };
        let newest = MathKey {
            latex: Arc::from("latex"),
            font_size: MAX_ENTRIES as u32 + 7,
            color: [0; 4],
        };
        assert!(!cache.entries.contains_key(&oldest));
        assert!(cache.entries.contains_key(&newest));
    }

    #[test]
    fn the_full_pipeline_rasterizes_a_formula_to_png() {
        let key = MathKey::new(r"\int_0^1 x^2 \, dx = \frac{1}{3}", 14.0, gpui::black());
        let (image, width, height) = render_formula(&key).expect("rendering should succeed");

        assert!(width > 0.0 && height > 0.0);
        assert_eq!(
            &image.bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "output must be PNG"
        );
        // 2× dpr: the bitmap is twice the logical layout size, so the source
        // must have produced a bitmap wider than a bare glyph or two.
        assert!(width * 2.0 >= 16.0);
    }

    #[test]
    fn malformed_latex_fails_without_panicking() {
        let key = MathKey::new(r"\frac{", 14.0, gpui::black());
        assert!(render_formula(&key).is_err());
    }
}
