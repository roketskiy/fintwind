//! Cached diagram images for Markdown fences.
//!
//! Frames only look up immutable results and queue work. Parsing, layout and
//! SVG rasterization happen on background workers; neither the source text nor
//! any I/O is touched by `render`.
//!
//! Failure modes this module is built around:
//!
//! - **Compile or raster failure.** Unsupported syntax, invalid SVG and
//!   renderer errors all resolve to one cached failure, so a broken fence
//!   never retries every frame and never paints a stale success.
//! - **Panics in third-party code.** The whole background job is wrapped in
//!   `catch_unwind`; a panic is treated exactly like a compile error.
//! - **Oversized diagrams.** The raster is bounded by both a pixel budget and
//!   an edge budget and is scaled down proportionally, so a huge logical
//!   canvas costs a bounded allocation while the image keeps its aspect ratio.
//!   Pathological aspect ratios that cannot produce even one pixel of height
//!   fail and fall back rather than allocate.
//! - **Cache pressure.** At most 64 results / 32 MiB of rasters are retained and
//!   16 jobs may be pending. Only resolved entries are evicted and a pending job
//!   is never dropped or overwritten by a newer request. The last committed
//!   request set stays protected across idle time; the next render's requests
//!   join that protection until its set commits. Transient image Arc references
//!   add conservative protection, but are not the cross-frame visibility test.
//! - **Admission refusal.** A completed raster that does not fit even after
//!   evicting every unpinned result is cached as a failure rather than kept
//!   over budget. That fence keeps showing its source: the refusal is cached
//!   like any other failure, so it never retries per frame and does not come
//!   back on its own when memory later frees up. While a result waits for
//!   admission, worker staging holds at most [`WORKERS`] extra rasters, each
//!   already bounded by the raster pixel budget.
//! - **Readiness probing.** `is_ready` answers a source toggle's question with
//!   the element's own key. The peek is read-only: it never inserts an entry,
//!   never queues work and never evicts, so asking cannot change the next frame.
//! - **Pending saturation.** When every pending slot is taken, the requesting
//!   view is parked and re-notified after the next completion instead of
//!   growing the queue without bound.
//! - **Theme, font-scale and monitor changes.** All three are part of the key,
//!   so a change recompiles instead of reusing an image with the wrong colors
//!   or density. Retired images are dropped from GPUI's atlas explicitly.
//! - **Row measurement drift.** A pending diagram measures as its source
//!   fallback; replacing it with an image changes the block height. This module
//!   notifies the requesting view on completion, and the GPUI list re-measures
//!   its visible rows every frame, so no external forced re-measure step is
//!   needed for the new height to be honoured.
//! - **Degenerate input.** Non-finite font scale or scale factor is clamped;
//!   empty or non-finite diagram dimensions fail instead of poisoning layout.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use gpui::{
    AnyElement, App, EntityId, Global, IntoElement, KeyDownEvent, ObjectFit, RenderImage,
    RenderOnce, SharedString, SvgRenderer, Window, div, img, prelude::*, px,
};

use fintwind_diagram::diagram_compile::{self, Theme};
use super::render::Palette;

/// Resolved results retained at once. Workers staging a result are not counted
/// here; their rasters are bounded by [`MAX_RASTER_PIXELS`] instead.
const MAX_ENTRIES: usize = 64;
/// Total raster bytes retained by resolved results.
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PENDING: usize = 16;
const WORKERS: usize = 2;
const BATCH_SIZE: usize = 1;

/// Logical pixel budget for one rasterized image.
const MAX_RASTER_PIXELS: f64 = 4_000_000.0;
/// Hard edge budget, matching the SVG renderer's own pixmap cap.
const MAX_RASTER_EDGE: f64 = 8192.0;
/// `SvgRenderer::render_single_frame` supersamples by this factor internally.
const SVG_SUPERSAMPLE: f64 = 2.0;
/// A single rendered image must keep its dimensions representable as `f32`.
const MAX_LOGICAL_EDGE: f64 = 1_000_000.0;

fn normalize_font_scale(font_scale: f32) -> f32 {
    if font_scale.is_finite() {
        font_scale.clamp(0.5, 4.0)
    } else {
        1.0
    }
}

fn normalize_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(1.0, 3.0)
    } else {
        1.0
    }
}

/// One Markdown diagram fence.
///
/// The digest is computed once, when the fence is parsed into the block tree,
/// so `Hash` never walks the source again. `Eq` compares the real bytes, which
/// keeps a digest collision from aliasing two different diagrams.
pub(super) struct Source {
    text: Arc<str>,
    digest: u64,
    reference_day: u64,
}

impl Source {
    pub(super) fn new(source: &str) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hasher);
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            text: Arc::from(source),
            digest: hasher.finish(),
            // Capture today once; theme and DPI changes retain the same Gantt date.
            reference_day: seconds / 86_400 * 86_400 * 1000,
        }
    }

    pub(super) fn as_str(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("bytes", &self.text.len())
            .field("digest", &self.digest)
            .finish()
    }
}

impl Hash for Source {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.digest.hash(state);
        self.reference_day.hash(state);
    }
}

impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        self.digest == other.digest
            && self.reference_day == other.reference_day
            && self.text == other.text
    }
}

impl Eq for Source {}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ThemeKey {
    background: [u8; 3],
    foreground: [u8; 3],
    muted: [u8; 3],
    border: [u8; 3],
    accent: [u8; 3],
}

impl ThemeKey {
    fn new(theme: &Theme) -> Self {
        Self {
            background: theme.background,
            foreground: theme.foreground,
            muted: theme.muted,
            border: theme.border,
            accent: theme.accent,
        }
    }
}

/// A diagram depends on its exact source, every theme color, the UI font
/// scale, and the display's scale factor.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Key {
    source: Arc<Source>,
    theme: ThemeKey,
    font_scale: u32,
    scale: u32,
}

impl Key {
    fn new(source: Arc<Source>, theme: &Theme, font_scale: f32, scale: f32) -> Self {
        Self {
            source,
            theme: ThemeKey::new(theme),
            font_scale: normalize_font_scale(font_scale).to_bits(),
            scale: normalize_scale(scale).to_bits(),
        }
    }

    fn font_scale(&self) -> f32 {
        f32::from_bits(self.font_scale)
    }

    fn scale(&self) -> f32 {
        f32::from_bits(self.scale)
    }
}

/// A compile or raster failure message, cached so a broken fence neither
/// retries every frame nor hides the reason from the reader.
type Failure = Arc<str>;

struct Rendered {
    image: Arc<RenderImage>,
    /// Diagram width in logical pixels before `font_scale` is applied.
    logical_width: f32,
    logical_height: f32,
}

#[derive(Clone)]
enum Lookup {
    Ready(Arc<Rendered>),
    Pending,
    Failed(Failure),
}

enum State {
    Pending(HashSet<EntityId>),
    // `Err` is a cached failure: malformed diagrams do not retry every frame.
    Ready(Result<Arc<Rendered>, Failure>),
}

struct Entry {
    state: State,
    touched: u64,
    bytes: usize,
}

impl Entry {
    /// Whether dropping this result would throw away a raster that `img` still
    /// references, forcing the same diagram to rasterize again next frame.
    ///
    /// The cache itself owns one reference to `Rendered.image`; any further
    /// reference means an element tree — this frame's, the previous one's, or an
    /// overdraw pass — still holds the image. That is a deliberate overestimate:
    /// keeping one stale raster for a frame costs memory, evicting a visible one
    /// costs a full compile.
    fn in_use(&self) -> bool {
        match &self.state {
            State::Ready(Ok(rendered)) => Arc::strong_count(&rendered.image) > 1,
            _ => false,
        }
    }
}

struct Job {
    key: Key,
    theme: Theme,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Entry>,
    queue: VecDeque<Job>,
    waiting: HashSet<EntityId>,
    clock: u64,
    bytes: usize,
    active: usize,
    pending: usize,
    retired: Vec<Arc<RenderImage>>,
    // Explicit frame requests outlive the transient `img` element tree. Arc
    // counts alone cannot protect the screen between two frames.
    visible: HashSet<Key>,
    requested: HashSet<Key>,
    commit_scheduled: bool,
}

// Interior mutability keeps cache hits from triggering GPUI's global-observer
// effects. The store belongs to the UI thread and never takes a blocking lock.
#[derive(Default)]
struct Store(RefCell<Cache>);
impl Global for Store {}

impl Cache {
    fn request(&mut self, key: &Key, theme: Theme, view: EntityId) -> Lookup {
        self.requested.insert(key.clone());
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(key) {
            entry.touched = self.clock;
            return match &mut entry.state {
                State::Pending(views) => {
                    views.insert(view);
                    Lookup::Pending
                }
                State::Ready(Ok(rendered)) => Lookup::Ready(rendered.clone()),
                State::Ready(Err(message)) => Lookup::Failed(message.clone()),
            };
        }
        if self.pending >= MAX_PENDING {
            self.waiting.insert(view);
            return Lookup::Pending;
        }
        // A pending entry holds no raster, but it still occupies an entry slot.
        // Free one without disturbing protected results. If every slot is
        // protected, keep the source instead of growing the map past its cap.
        while self.entries.len() >= MAX_ENTRIES {
            if !self.evict_oldest_unused(std::slice::from_ref(key)) {
                // No slot without disturbing this frame. Keep the source; no
                // compile is started, and a later frame can try admission
                // again. Pending, not Failed: cache pressure is not a
                // diagnostic the reader should see.
                self.waiting.insert(view);
                return Lookup::Pending;
            }
        }
        self.entries.insert(
            key.clone(),
            Entry {
                state: State::Pending(HashSet::from([view])),
                touched: self.clock,
                bytes: 0,
            },
        );
        self.queue.push_back(Job {
            key: key.clone(),
            theme,
        });
        self.pending += 1;
        Lookup::Pending
    }

    /// Drop the least recently shown result that is neither retained by the
    /// caller nor still referenced by an `img`.
    ///
    /// Returns `false` when every resolved entry is in use, which is the only
    /// case where the cache is allowed to stay over its budgets: the alternative
    /// would be to evict something that is on screen.
    fn evict_oldest_unused(&mut self, retain: &[Key]) -> bool {
        let oldest = self
            .entries
            .iter()
            .filter(|(key, entry)| {
                matches!(entry.state, State::Ready(_))
                    && !retain.contains(*key)
                    && !self.visible.contains(*key)
                    && !self.requested.contains(*key)
                    && !entry.in_use()
            })
            .min_by_key(|(_, entry)| entry.touched)
            .map(|(key, _)| key.clone());
        let Some(key) = oldest else { return false };
        let entry = self.entries.remove(&key).expect("entry was just selected");
        self.bytes = self.bytes.saturating_sub(entry.bytes);
        if let State::Ready(Ok(rendered)) = entry.state {
            self.retired.push(rendered.image.clone());
        }
        true
    }

    fn trim(&mut self, retain: &[Key]) {
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_CACHE_BYTES {
            if !self.evict_oldest_unused(retain) {
                break;
            }
        }
    }

    /// Make room for one completed raster of `bytes`.
    ///
    /// Eviction stops at pinned and freshly completed results, so a full cache
    /// fails the admission instead of evicting an image that is still on screen.
    /// Rasters larger than the whole budget are refused without touching the
    /// cache at all.
    fn admit(&mut self, retain: &[Key], bytes: usize) -> bool {
        if bytes > MAX_CACHE_BYTES {
            return false;
        }
        while self.bytes + bytes > MAX_CACHE_BYTES && self.evict_oldest_unused(retain) {}
        self.bytes + bytes <= MAX_CACHE_BYTES
    }

    fn complete(
        &mut self,
        results: Vec<(Key, Result<Arc<Rendered>, Failure>)>,
    ) -> HashSet<EntityId> {
        let mut notify = std::mem::take(&mut self.waiting);
        // Results resolved in this batch are fresh but not yet referenced by an
        // `img`, so they are retained explicitly until the next frame can pin
        // them; admitting one must not evict another from the same batch.
        let mut completed: Vec<Key> = Vec::with_capacity(results.len());
        for (key, result) in results {
            self.pending = self.pending.saturating_sub(1);
            let Some(mut entry) = self.entries.remove(&key) else {
                continue;
            };
            // Source, theme, font scale and display scale are part of the
            // immutable key, so an old completion cannot overwrite a newer
            // diagram, and a pending entry is never replaced.
            let State::Pending(views) = &mut entry.state else {
                self.entries.insert(key, entry);
                continue;
            };
            notify.extend(views.drain());
            self.bytes = self.bytes.saturating_sub(entry.bytes);
            entry.bytes = result
                .as_ref()
                .ok()
                .map_or(0, |rendered| {
                    rendered.image.as_bytes(0).map_or(0, |bytes| bytes.len())
                });
            let state = match result {
                Ok(rendered) if self.admit(&completed, entry.bytes) => {
                    self.bytes += entry.bytes;
                    Ok(rendered)
                }
                // No room without dropping an image that is still visible.
                // Caching the refusal keeps the fence on its source instead of
                // recompiling the same oversized diagram every frame.
                Ok(rendered) => {
                    entry.bytes = 0;
                    self.retired.push(rendered.image.clone());
                    Err(Arc::from("diagram raster exceeds the memory budget"))
                }
                Err(message) => Err(message),
            };
            entry.state = State::Ready(state);
            self.clock += 1;
            entry.touched = self.clock;
            completed.push(key.clone());
            self.entries.insert(key, entry);
        }
        self.trim(&completed);
        notify
    }
}

/// Render one diagram, reusing a cached image when it is ready.
pub(super) fn element(
    source: Arc<Source>,
    theme: Theme,
    font_scale: f32,
    palette: Palette,
    fallback: AnyElement,
    element_id: SharedString,
) -> AnyElement {
    DiagramImage {
        source,
        theme,
        font_scale: normalize_font_scale(font_scale),
        palette,
        fallback,
        element_id,
    }
    .into_any_element()
}

/// The exact key `element` uses for a fence, so other read-only probes cannot
/// drift from what the renderer looks up.
fn key_for(source: Arc<Source>, theme: &Theme, font_scale: f32, window: &Window) -> Key {
    Key::new(source, theme, font_scale, window.scale_factor())
}

/// A user asked to expand a cached diagram into the fullscreen preview.
/// Stored globally because the clickable image renders without access to the
/// owning view: the click records the request and notifies the owner, and the
/// app drains it on its next frame.
#[derive(Clone)]
pub struct PreviewRequest {
    pub image: Arc<RenderImage>,
    /// Base display size in logical pixels (`logical * font_scale`).
    pub width: f32,
    pub height: f32,
}

struct PendingPreview(RefCell<Option<PreviewRequest>>);
impl Global for PendingPreview {}

pub fn request_preview(request: PreviewRequest, cx: &mut App) {
    if !cx.has_global::<PendingPreview>() {
        cx.set_global(PendingPreview(RefCell::new(None)));
    }
    *cx.global::<PendingPreview>().0.borrow_mut() = Some(request);
}

pub fn take_preview_request(cx: &mut App) -> Option<PreviewRequest> {
    let pending = cx.try_global::<PendingPreview>()?;
    pending.0.borrow_mut().take()
}

/// Read-only peek returning the cached image for the exact key [`element`]
/// uses, so an expand button cannot drift from what the renderer paints.
pub(super) fn preview_image(
    source: Arc<Source>,
    theme: Theme,
    font_scale: f32,
    window: &Window,
    cx: &App,
) -> Option<PreviewRequest> {
    let key = key_for(source, &theme, font_scale, window);
    let store = cx.try_global::<Store>()?;
    match store.0.borrow().entries.get(&key) {
        Some(Entry {
            state: State::Ready(Ok(rendered)),
            ..
        }) => {
            let width = (rendered.logical_width * key.font_scale()).max(1.0);
            let height = (rendered.logical_height * key.font_scale()).max(1.0);
            Some(PreviewRequest {
                image: rendered.image.clone(),
                width,
                height,
            })
        }
        _ => None,
    }
}

/// Whether [`element`] would paint a cached image right now, using the same key
/// and nothing else.
///
/// This is a read-only peek: it never inserts an entry, starts a job or evicts,
/// so a toggle may ask on every frame without changing what renders. A fence
/// that is still pending, has failed, or was refused admission reports `false`,
/// which is exactly when [`element`] shows its source fallback.
pub(super) fn is_ready(
    source: Arc<Source>,
    theme: Theme,
    font_scale: f32,
    window: &Window,
    cx: &App,
) -> bool {
    let key = key_for(source, &theme, font_scale, window);
    let Some(store) = cx.try_global::<Store>() else {
        return false;
    };
    matches!(
        store.0.borrow().entries.get(&key),
        Some(Entry {
            state: State::Ready(Ok(_)),
            ..
        })
    )
}

#[derive(IntoElement)]
struct DiagramImage {
    source: Arc<Source>,
    theme: Theme,
    font_scale: f32,
    palette: Palette,
    fallback: AnyElement,
    element_id: SharedString,
}

impl RenderOnce for DiagramImage {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let key = key_for(self.source, &self.theme, self.font_scale, window);
        match request(key, self.theme, window.current_view(), cx) {
            Lookup::Ready(rendered) => {
                let width = (rendered.logical_width * self.font_scale).max(1.0);
                let height = (rendered.logical_height * self.font_scale).max(1.0);
                let owner = window.current_view();
                let preview = PreviewRequest {
                    image: rendered.image.clone(),
                    width,
                    height,
                };
                let keyboard_preview = preview.clone();
                let keyboard_owner = owner;
                div()
                    .id(self.element_id)
                    .tab_index(0)
                    .relative()
                    .w(px(width))
                    .max_w_full()
                    // A diagram narrower than the column reads centered, like
                    // every other media row; a wide one fills the column and
                    // the auto margins collapse to zero.
                    .mx_auto()
                    .aspect_ratio(rendered.logical_width / rendered.logical_height)
                    .flex_shrink_0()
                    .cursor_default()
                    .focus_visible(|style| {
                        style.border_1().border_color(self.palette.accent)
                    })
                    .child(
                        img(rendered.image.clone())
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    )
                    .on_click(move |_, _, cx| {
                        request_preview(preview.clone(), cx);
                        cx.notify(owner);
                    })
                    .on_key_down(move |event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            request_preview(keyboard_preview.clone(), cx);
                            cx.notify(keyboard_owner);
                        }
                    })
                    .into_any_element()
            }
            // The source stays visible until an image exists, and a cached
            // failure keeps showing it — with the reason — instead of an
            // empty box.
            Lookup::Failed(message) => div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap(px(6.0))
                        .child(crate::ui::icon(
                            "icons/alert.svg",
                            12.0,
                            self.palette.removed,
                        ))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_size(px(11.0))
                                .line_height(px(16.0))
                                .text_color(self.palette.removed)
                                .child(SharedString::from(message.to_string())),
                        ),
                )
                .child(self.fallback)
                .into_any_element(),
            Lookup::Pending => self.fallback,
        }
    }
}

fn request(key: Key, theme: Theme, view: EntityId, cx: &mut App) -> Lookup {
    if !cx.has_global::<Store>() {
        cx.set_global(Store::default());
    }
    let (lookup, commit) = {
        let retained = std::slice::from_ref(&key);
        let mut cache = cx.global::<Store>().0.borrow_mut();
        let lookup = cache.request(&key, theme, view);
        cache.trim(retained);
        let commit = !cache.commit_scheduled;
        cache.commit_scheduled = true;
        (lookup, commit)
    };
    retire_images(cx);
    if commit {
        // Commit the union of requests only after every visible fence has had
        // a chance to register. Keep it across idle time between frames.
        cx.defer(|cx| {
            let mut cache = cx.global::<Store>().0.borrow_mut();
            let changed = cache.visible != cache.requested;
            cache.visible = std::mem::take(&mut cache.requested);
            cache.commit_scheduled = false;
            cache.trim(&[]);
            let retry = if changed {
                std::mem::take(&mut cache.waiting)
            } else {
                HashSet::new()
            };
            drop(cache);
            for view in retry {
                cx.notify(view);
            }
            retire_images(cx);
            pump(cx);
        });
    }
    lookup
}

fn retire_images(cx: &mut App) {
    let images = std::mem::take(&mut cx.global::<Store>().0.borrow_mut().retired);
    if !images.is_empty() {
        // Dropping the Arc alone does not remove GPUI's atlas entry. Do that
        // explicitly after the frame, as GPUI's native image caches do.
        cx.defer(move |cx| {
            for image in images {
                cx.drop_image(image, None);
            }
        });
    }
}

fn pump(cx: &mut App) {
    loop {
        let batch = {
            let mut cache = cx.global::<Store>().0.borrow_mut();
            if cache.active >= WORKERS || cache.queue.is_empty() {
                return;
            }
            cache.active += 1;
            let count = BATCH_SIZE.min(cache.queue.len());
            cache.queue.drain(..count).collect::<Vec<_>>()
        };
        let renderer = cx.svg_renderer();
        let task = cx.background_executor().spawn(async move {
            batch
                .into_iter()
                .map(|Job { key, theme }| {
                    let result: Result<Arc<Rendered>, Failure> =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            rasterize(&key, theme, &renderer)
                        }))
                        .unwrap_or_else(|_| {
                            Err(anyhow::anyhow!("diagram compilation panicked"))
                        })
                        .map(|rendered| Arc::new(rendered))
                        .map_err(|error| failure_message(&error));
                    (key, result)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |cx| {
            let results = task.await;
            let _ = cx.update(|cx| {
                let views = {
                    let mut cache = cx.global::<Store>().0.borrow_mut();
                    cache.active = cache.active.saturating_sub(1);
                    cache.complete(results)
                };
                for view in views {
                    cx.notify(view);
                }
                retire_images(cx);
                pump(cx);
            });
        })
        .detach();
    }
}

/// A bounded, single-line reason for a cached failure. The full `anyhow`
/// chain stays useful in logs; a fence shows the cause compactly. Truncation
/// lands on a char boundary because a label may be any UTF-8.
fn failure_message(error: &anyhow::Error) -> Failure {
    const MAX_FAILURE_BYTES: usize = 400;
    let text = format!("{error:#}");
    if text.len() <= MAX_FAILURE_BYTES {
        return Arc::from(text);
    }
    let mut end = MAX_FAILURE_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    Arc::from(&text[..end])
}

fn rasterize(key: &Key, theme: Theme, renderer: &SvgRenderer) -> anyhow::Result<Rendered> {
    let diagram =
        diagram_compile::compile(key.source.as_str(), theme, key.source.reference_day as f64)?;
    let width = diagram.width;
    let height = diagram.height;
    anyhow::ensure!(
        width.is_finite()
            && height.is_finite()
            && width > 0.0
            && height > 0.0
            && width <= MAX_LOGICAL_EDGE
            && height <= MAX_LOGICAL_EDGE,
        "diagram dimensions out of range"
    );

    let scale = raster_scale(width, height, key.font_scale(), key.scale())?;
    let image = renderer.render_single_frame(diagram.svg.as_bytes(), scale)?;
    anyhow::ensure!(
        image.frame_count() > 0 && image.size(0).width.0 > 0 && image.size(0).height.0 > 0,
        "empty diagram raster"
    );
    Ok(Rendered {
        image,
        logical_width: width as f32,
        logical_height: height as f32,
    })
}

/// Pick the density passed to the SVG renderer.
///
/// The renderer multiplies it by [`SVG_SUPERSAMPLE`], so the resulting pixmap
/// is `logical * 2 * scale`. Both the pixel budget and the edge budget bound
/// that product; the contract between width and height is never changed, which
/// is what lets the `img` element derive the displayed height from the raster's
/// intrinsic aspect ratio.
fn raster_scale(
    width: f64,
    height: f64,
    font_scale: f32,
    display_scale: f32,
) -> anyhow::Result<f32> {
    let density = (f64::from(font_scale) * f64::from(display_scale)).clamp(0.5, 4.0);
    let edge = MAX_RASTER_EDGE / (SVG_SUPERSAMPLE * width.max(height));
    let area = (MAX_RASTER_PIXELS / (SVG_SUPERSAMPLE * SVG_SUPERSAMPLE * width * height)).sqrt();
    let scale = density.min(edge).min(area);
    anyhow::ensure!(
        scale.is_finite()
            && scale > 0.0
            && width * SVG_SUPERSAMPLE * scale >= 1.0
            && height * SVG_SUPERSAMPLE * scale >= 1.0,
        "diagram raster limit"
    );
    Ok(scale as f32)
}
