//! Window-modal preview for images and Mermaid diagrams.
//!
//! Hosted in its own `Entity<ImagePreviewModal>` so high-frequency pan/zoom
//! updates repaint only the modal layer while Fintwind's cached workspace panes
//! replay without rebuilding.

use std::cell::{Cell, RefCell};
use std::io::Cursor;
use std::rc::Rc;

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    data_display::{Badge, Tone},
    primitives::{FocusScope, Icon, IconName, Tooltip as ElyTooltip},
    theme::{ActiveTheme as _, ControlSize, Elevation, TextSize},
};
use gpui::{
    Animation, AnimationExt, App, ClipboardItem, CursorStyle, KeyBinding, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, PinchEvent, Pixels, Point, Render,
    RenderImage, ScrollDelta, ScrollHandle, ScrollWheelEvent, SharedString, Size, actions, canvas,
    div, img, point, px,
};

use super::*;

actions!(
    fintwind_image_preview,
    [
        DismissImagePreview,
        ImagePreviewPrev,
        ImagePreviewNext,
        ImagePreviewZoomIn,
        ImagePreviewZoomOut,
        ImagePreviewFit,
        ImagePreviewActualSize,
        ImagePreviewRotateCw,
        ImagePreviewRotateCcw,
        ImagePreviewCopy,
        ImagePreviewSaveAs
    ]
);

pub(super) const IMAGE_PREVIEW_LAYER_PRIORITY: usize = 5;
const IMAGE_PREVIEW_CONTEXT: &str = "ImagePreview";
const IMAGE_PREVIEW_ANIMATION_DURATION: Duration = Duration::from_millis(140);
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 8.0;
const ZOOM_STEP: f32 = 1.25;
const WHEEL_ZOOM_STEP: f32 = 1.12;
const CANVAS_PADDING: f32 = 24.0;
const SIDE_NAV_CLEARANCE: f32 = 60.0;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(
            "escape",
            DismissImagePreview,
            Some(IMAGE_PREVIEW_CONTEXT),
        ),
        KeyBinding::new("left", ImagePreviewPrev, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("right", ImagePreviewNext, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("ctrl-=", ImagePreviewZoomIn, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("ctrl-+", ImagePreviewZoomIn, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("ctrl--", ImagePreviewZoomOut, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("ctrl-0", ImagePreviewFit, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("ctrl-1", ImagePreviewActualSize, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("r", ImagePreviewRotateCw, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("shift-r", ImagePreviewRotateCcw, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("ctrl-c", ImagePreviewCopy, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("ctrl-s", ImagePreviewSaveAs, Some(IMAGE_PREVIEW_CONTEXT)),
    ]);
}

#[derive(Clone)]
pub(super) enum ImagePreviewSource {
    Image(Arc<gpui::Image>),
    Render {
        image: Arc<RenderImage>,
        logical_width: f32,
        logical_height: f32,
    },
}

#[derive(Clone)]
pub(super) struct ImagePreviewItem {
    pub source: ImagePreviewSource,
    pub name: SharedString,
    pub path: Option<PathBuf>,
    pub can_reveal: bool,
}

impl ImagePreviewItem {
    pub(super) fn from_image(
        image: Arc<gpui::Image>,
        name: SharedString,
        path: Option<PathBuf>,
        can_reveal: bool,
    ) -> Self {
        Self {
            source: ImagePreviewSource::Image(image),
            name,
            path,
            can_reveal,
        }
    }

    pub(super) fn from_diagram(
        request: crate::md::diagram::PreviewRequest,
        name: SharedString,
    ) -> Self {
        Self {
            source: ImagePreviewSource::Render {
                image: request.image,
                logical_width: request.width.max(1.0),
                logical_height: request.height.max(1.0),
                },
            name,
            path: None,
            can_reveal: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum ImagePreviewZoomMode {
    Fit,
    Scale(f32),
}

struct ImageSlideState {
    item: ImagePreviewItem,
    /// Intrinsic pixel dimensions of the original unrotated image (for the badge).
    pixel_dimensions: Cell<Option<(u32, u32)>>,
    /// Logical 1:1 base size of the unrotated image (for layout & 1:1 scaling).
    logical_dimensions: Cell<Option<(f32, f32)>>,
    /// Decoded RenderImage of the unrotated image, captured once so subsequent
    /// rotations or diagram exports never lose source pixels.
    base_render_image: RefCell<Option<Arc<RenderImage>>>,
    /// Clockwise quarter turns (`0..4`).
    rotation: u8,
    /// Cached encoded images per rotation. `[0]` is populated up-front for
    /// `Image` sources and lazily on export/copy for `Render` diagram sources.
    encoded: [Option<Arc<gpui::Image>>; 4],
    processing: bool,
}

impl ImageSlideState {
    fn new(item: ImagePreviewItem) -> Self {
        let (pixel_dims, logical_dims, base_render, encoded_zero) = match &item.source {
            ImagePreviewSource::Image(image) => (None, None, None, Some(image.clone())),
            ImagePreviewSource::Render {
                image,
                logical_width,
                logical_height,
            } => {
                let px_size = image.size(0);
                let pixel_dims = (px_size.width.0 > 0 && px_size.height.0 > 0)
                    .then_some((px_size.width.0 as u32, px_size.height.0 as u32));
                (
                    pixel_dims,
                    Some((*logical_width, *logical_height)),
                    Some(image.clone()),
                    None,
                )
            }
        };
        Self {
            item,
            pixel_dimensions: Cell::new(pixel_dims),
            logical_dimensions: Cell::new(logical_dims),
            base_render_image: RefCell::new(base_render),
            rotation: 0,
            encoded: [encoded_zero, None, None, None],
            processing: false,
        }
    }

    fn display_source(&self) -> ImagePreviewSource {
        let slot = usize::from(self.rotation % 4);
        if slot == 0 {
            return self.item.source.clone();
        }
        if let Some(image) = &self.encoded[slot] {
            ImagePreviewSource::Image(image.clone())
        } else {
            self.item.source.clone()
        }
    }

    fn encoded_image_for_current_rotation(&self) -> Option<Arc<gpui::Image>> {
        self.encoded[usize::from(self.rotation % 4)].clone()
    }

    fn oriented_pixel_dimensions(&self) -> Option<(u32, u32)> {
        let (width, height) = self.pixel_dimensions.get()?;
        if self.rotation % 2 == 1 {
            Some((height, width))
        } else {
            Some((width, height))
        }
    }

    fn oriented_logical_dimensions(&self) -> Option<(f32, f32)> {
        let (width, height) = self.logical_dimensions.get()?;
        if self.rotation % 2 == 1 {
            Some((height, width))
        } else {
            Some((width, height))
        }
    }

    fn is_unavailable(&self) -> bool {
        match &self.item.source {
            ImagePreviewSource::Image(image) => image.bytes.is_empty(),
            ImagePreviewSource::Render { image, .. } => image.frame_count() == 0,
        }
    }
}

#[derive(Clone, Copy)]
struct DragPanState {
    start_mouse: Point<Pixels>,
    start_offset: Point<Pixels>,
}

pub(super) type CloseCallback = Rc<dyn Fn(Option<FocusHandle>, &mut Window, &mut App)>;
pub(super) type NotifyCallback = Rc<dyn Fn(String, bool, &mut App)>;
pub(super) type RevealCallback = Rc<dyn Fn(&Path, &mut App)>;

pub(super) struct ImagePreviewCallbacks {
    pub on_close: CloseCallback,
    pub on_notify: NotifyCallback,
    pub on_reveal: RevealCallback,
}

pub(super) struct ImagePreviewModal {
    slides: Vec<ImageSlideState>,
    current_index: usize,
    zoom_mode: ImagePreviewZoomMode,
    scroll_handle: ScrollHandle,
    viewport_size: Cell<Size<Pixels>>,
    drag: Option<DragPanState>,
    focus: FocusHandle,
    scope_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    generation: u64,
    copied_generation: u64,
    saving: bool,
    callbacks: ImagePreviewCallbacks,
}

impl ImagePreviewModal {
    pub(super) fn new(
        items: Vec<ImagePreviewItem>,
        initial_index: usize,
        generation: u64,
        previous_focus: Option<FocusHandle>,
        callbacks: ImagePreviewCallbacks,
        cx: &mut Context<Self>,
    ) -> Self {
        let current_index = initial_index.min(items.len().saturating_sub(1));
        Self {
            slides: items.into_iter().map(ImageSlideState::new).collect(),
            current_index,
            zoom_mode: ImagePreviewZoomMode::Fit,
            scroll_handle: ScrollHandle::new(),
            viewport_size: Cell::new(Size::default()),
            drag: None,
            focus: cx.focus_handle(),
            scope_focus: cx.focus_handle(),
            previous_focus,
            generation,
            copied_generation: 0,
            saving: false,
            callbacks,
        }
    }

    pub(super) fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    fn current_slide(&self) -> &ImageSlideState {
        &self.slides[self.current_index]
    }

    fn fit_scale(&self) -> f32 {
        let Some((width, height)) = self.current_slide().oriented_logical_dimensions() else {
            return 1.0;
        };
        let viewport = self.viewport_size.get();
        let avail_w = (f32::from(viewport.width) - CANVAS_PADDING * 2.0).max(1.0);
        let avail_h = (f32::from(viewport.height) - CANVAS_PADDING * 2.0).max(1.0);
        if viewport.width <= px(1.0) || viewport.height <= px(1.0) {
            return 1.0;
        }
        (avail_w / width)
            .min(avail_h / height)
            .min(1.0)
            .clamp(MIN_ZOOM, MAX_ZOOM)
    }

    pub(super) fn effective_scale(&self) -> f32 {
        match self.zoom_mode {
            ImagePreviewZoomMode::Fit => self.fit_scale(),
            ImagePreviewZoomMode::Scale(scale) => scale,
        }
    }

    fn wrap_dimensions(&self, scale: f32) -> (f32, f32) {
        let viewport = self.viewport_size.get();
        let vw = f32::from(viewport.width);
        let vh = f32::from(viewport.height);
        let Some((nat_w, nat_h)) = self.current_slide().oriented_logical_dimensions() else {
            return (vw, vh);
        };
        let display_w = (nat_w * scale).clamp(16.0, 16384.0);
        let display_h = (nat_h * scale).clamp(16.0, 16384.0);
        (
            vw.max(display_w + CANVAS_PADDING * 2.0),
            vh.max(display_h + CANVAS_PADDING * 2.0),
        )
    }

    fn apply_zoom_centered(&mut self, next_scale: f32) {
        let viewport = self.viewport_size.get();
        let vw = f32::from(viewport.width);
        let vh = f32::from(viewport.height);
        if vw <= 1.0 || vh <= 1.0 {
            self.zoom_mode = ImagePreviewZoomMode::Scale(next_scale);
            self.scroll_handle.set_offset(Point::default());
            return;
        }

        let old_scale = self.effective_scale();
        let (old_wrap_w, old_wrap_h) = self.wrap_dimensions(old_scale);
        let old_offset = self.scroll_handle.offset();

        let center_x_ratio = if matches!(self.zoom_mode, ImagePreviewZoomMode::Fit)
            || old_wrap_w <= vw + 0.5
        {
            0.5
        } else {
            ((-f32::from(old_offset.x) + vw * 0.5) / old_wrap_w).clamp(0.0, 1.0)
        };
        let center_y_ratio = if matches!(self.zoom_mode, ImagePreviewZoomMode::Fit)
            || old_wrap_h <= vh + 0.5
        {
            0.5
        } else {
            ((-f32::from(old_offset.y) + vh * 0.5) / old_wrap_h).clamp(0.0, 1.0)
        };

        self.zoom_mode = ImagePreviewZoomMode::Scale(next_scale);
        let (new_wrap_w, new_wrap_h) = self.wrap_dimensions(next_scale);
        let next_x = if new_wrap_w > vw + 0.5 {
            let max_x = new_wrap_w - vw;
            -(center_x_ratio * new_wrap_w - vw * 0.5).clamp(0.0, max_x)
        } else {
            0.0
        };
        let next_y = if new_wrap_h > vh + 0.5 {
            let max_y = new_wrap_h - vh;
            -(center_y_ratio * new_wrap_h - vh * 0.5).clamp(0.0, max_y)
        } else {
            0.0
        };
        self.scroll_handle.set_offset(point(px(next_x), px(next_y)));
    }

    pub(super) fn step(&mut self, delta: isize) -> bool {
        let next = self.current_index as isize + delta;
        if next < 0 || next >= self.slides.len() as isize {
            return false;
        }
        self.current_index = next as usize;
        self.zoom_mode = ImagePreviewZoomMode::Fit;
        self.drag = None;
        self.scroll_handle.set_offset(Point::default());
        true
    }

    pub(super) fn zoom(&mut self, factor: f32) -> bool {
        if self.current_slide().is_unavailable() {
            return false;
        }
        let next = (self.effective_scale() * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.apply_zoom_centered(next);
        true
    }

    pub(super) fn set_fit(&mut self) {
        self.zoom_mode = ImagePreviewZoomMode::Fit;
        self.drag = None;
        self.scroll_handle.set_offset(Point::default());
    }

    pub(super) fn set_actual_size(&mut self) -> bool {
        if self.current_slide().is_unavailable() {
            return false;
        }
        self.apply_zoom_centered(1.0);
        self.drag = None;
        true
    }

    pub(super) fn toggle_fit_or_actual(&mut self) -> bool {
        match self.zoom_mode {
            ImagePreviewZoomMode::Fit => self.set_actual_size(),
            ImagePreviewZoomMode::Scale(_) => {
                self.set_fit();
                true
            }
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.previous_focus.take();
        (self.callbacks.on_close)(previous, window, cx);
    }

    fn rotate(
        &mut self,
        quarter_turns_delta: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slide_index = self.current_index;
        let slide = &mut self.slides[slide_index];
        if slide.processing || slide.is_unavailable() {
            return;
        }
        let target_rotation = (slide.rotation + quarter_turns_delta) % 4;
        if slide.encoded[usize::from(target_rotation)].is_some()
            || (target_rotation == 0
                && matches!(slide.item.source, ImagePreviewSource::Render { .. }))
        {
            slide.rotation = target_rotation;
            self.drag = None;
            self.scroll_handle.set_offset(Point::default());
            cx.notify();
            return;
        }

        let Some(render_image) = slide.base_render_image.borrow().clone().or_else(|| {
            if let ImagePreviewSource::Image(base) = &slide.item.source {
                base.clone().get_render_image(window, cx)
            } else {
                None
            }
        }) else {
            (self.callbacks.on_notify)(tr!("image_preview.rotate_failed"), false, cx);
            return;
        };

        let px_size = render_image.size(0);
        let width = px_size.width.0.max(0) as u32;
        let height = px_size.height.0.max(0) as u32;
        let Some(bgra_bytes) = render_image.as_bytes(0).map(|bytes| bytes.to_vec()) else {
            (self.callbacks.on_notify)(tr!("image_preview.rotate_failed"), false, cx);
            return;
        };
        if width == 0 || height == 0 {
            (self.callbacks.on_notify)(tr!("image_preview.rotate_failed"), false, cx);
            return;
        }

        slide.processing = true;
        cx.notify();

        cx.spawn(async move |modal, cx| {
            let rotated_image = cx
                .background_executor()
                .spawn(async move {
                    encode_rotated_png(&bgra_bytes, width, height, target_rotation).map(
                        |png_bytes| {
                            Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png_bytes))
                        },
                    )
                })
                .await;

            let _ = modal.update(cx, |this, cx| {
                let mut failed = false;
                if let Some(slide) = this.slides.get_mut(slide_index) {
                    slide.processing = false;
                    if let Some(rotated) = rotated_image {
                        slide.encoded[usize::from(target_rotation)] = Some(rotated);
                        slide.rotation = target_rotation;
                        this.drag = None;
                        this.scroll_handle.set_offset(Point::default());
                    } else {
                        failed = true;
                    }
                }
                if failed {
                    (this.callbacks.on_notify)(tr!("image_preview.rotate_failed"), false, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_current(&mut self, cx: &mut Context<Self>) {
        let slide_index = self.current_index;
        let slide = &mut self.slides[slide_index];
        if slide.is_unavailable() || slide.processing {
            return;
        }
        if let Some(image) = slide.encoded_image_for_current_rotation() {
            self.finish_copy_image(image, cx);
            return;
        }

        let Some(render_image) = slide.base_render_image.borrow().clone() else {
            (self.callbacks.on_notify)(tr!("image_preview.copy_failed"), false, cx);
            return;
        };
        let px_size = render_image.size(0);
        let width = px_size.width.0.max(0) as u32;
        let height = px_size.height.0.max(0) as u32;
        let Some(bgra_bytes) = render_image.as_bytes(0).map(|bytes| bytes.to_vec()) else {
            (self.callbacks.on_notify)(tr!("image_preview.copy_failed"), false, cx);
            return;
        };
        if width == 0 || height == 0 {
            (self.callbacks.on_notify)(tr!("image_preview.copy_failed"), false, cx);
            return;
        }

        let rotation = slide.rotation;
        slide.processing = true;
        cx.notify();

        cx.spawn(async move |modal, cx| {
            let encoded = cx
                .background_executor()
                .spawn(async move {
                    encode_rotated_png(&bgra_bytes, width, height, rotation).map(|png_bytes| {
                        Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png_bytes))
                    })
                })
                .await;
            let _ = modal.update(cx, |this, cx| {
                if let Some(slide) = this.slides.get_mut(slide_index) {
                    slide.processing = false;
                    if let Some(image) = encoded {
                        slide.encoded[usize::from(rotation % 4)] = Some(image.clone());
                        if this.current_index == slide_index {
                            this.finish_copy_image(image, cx);
                            return;
                        }
                    } else {
                        (this.callbacks.on_notify)(tr!("image_preview.copy_failed"), false, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_copy_image(&mut self, image: Arc<gpui::Image>, cx: &mut Context<Self>) {
        if image.bytes.is_empty() {
            (self.callbacks.on_notify)(tr!("image_preview.copy_failed"), false, cx);
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_image(&image));
        self.copied_generation = self.copied_generation.wrapping_add(1);
        let copied_generation = self.copied_generation;
        (self.callbacks.on_notify)(tr!("image_preview.copied_toast"), true, cx);
        cx.notify();

        cx.spawn(async move |modal, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            let _ = modal.update(cx, |this, cx| {
                if this.copied_generation == copied_generation {
                    this.copied_generation = this.copied_generation.wrapping_add(1);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save_current_as(&mut self, cx: &mut Context<Self>) {
        if self.saving || self.current_slide().is_unavailable() || self.current_slide().processing {
            return;
        }
        let slide_index = self.current_index;
        let slide = &self.slides[slide_index];
        let rotation = slide.rotation;
        let encoded = slide.encoded_image_for_current_rotation();
        let default_dir = slide
            .item
            .path
            .as_deref()
            .and_then(Path::parent)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(default_save_directory);

        if let Some(image) = encoded {
            if image.bytes.is_empty() {
                return;
            }
            let suggested_name = suggested_save_filename(
                slide.item.name.as_ref(),
                image.format,
                rotation != 0,
            );
            self.prompt_and_write_bytes(default_dir, suggested_name, image.bytes.clone(), cx);
            return;
        }

        // Diagram (`Render`) source at rotation 0: encode PNG on a worker first.
        let Some(render_image) = slide.base_render_image.borrow().clone() else {
            return;
        };
        let px_size = render_image.size(0);
        let width = px_size.width.0.max(0) as u32;
        let height = px_size.height.0.max(0) as u32;
        let Some(bgra_bytes) = render_image.as_bytes(0).map(|bytes| bytes.to_vec()) else {
            return;
        };
        if width == 0 || height == 0 {
            return;
        }
        let suggested_name = suggested_save_filename(
            slide.item.name.as_ref(),
            gpui::ImageFormat::Png,
            true,
        );
        self.saving = true;
        cx.notify();

        cx.spawn(async move |modal, cx| {
            let encoded = cx
                .background_executor()
                .spawn(async move {
                    encode_rotated_png(&bgra_bytes, width, height, rotation).map(|png_bytes| {
                        Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png_bytes))
                    })
                })
                .await;
            let _ = modal.update(cx, |this, cx| {
                this.saving = false;
                if let Some(image) = encoded {
                    if let Some(slide) = this.slides.get_mut(slide_index) {
                        slide.encoded[usize::from(rotation % 4)] = Some(image.clone());
                    }
                    this.prompt_and_write_bytes(
                        default_dir,
                        suggested_name,
                        image.bytes.clone(),
                        cx,
                    );
                } else {
                    (this.callbacks.on_notify)(
                        tr!("image_preview.save_failed", error = "PNG encode failed"),
                        false,
                        cx,
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn prompt_and_write_bytes(
        &mut self,
        default_dir: PathBuf,
        suggested_name: String,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        self.saving = true;
        cx.notify();

        let receiver = cx.prompt_for_new_path(&default_dir, Some(&suggested_name));
        cx.spawn(async move |modal, cx| {
            let chosen = receiver.await.ok().and_then(Result::ok).flatten();
            let write_result = match chosen {
                Some(path) => {
                    let save_path = path.clone();
                    cx.background_executor()
                        .spawn(async move { std::fs::write(&save_path, &bytes).map(|()| save_path) })
                        .await
                        .map(Some)
                }
                None => Ok(None),
            };

            let _ = modal.update(cx, |this, cx| {
                this.saving = false;
                match write_result {
                    Ok(Some(saved_path)) => {
                        (this.callbacks.on_notify)(
                            tr!(
                                "image_preview.saved_toast",
                                path = saved_path.display().to_string()
                            ),
                            true,
                            cx,
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        (this.callbacks.on_notify)(
                            tr!("image_preview.save_failed", error = error.to_string()),
                            false,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reveal_current(&self, cx: &mut Context<Self>) {
        let item = &self.current_slide().item;
        let Some(path) = item.path.as_deref() else {
            return;
        };
        if item.can_reveal && !path.as_os_str().is_empty() {
            (self.callbacks.on_reveal)(path, cx);
        }
    }

    #[cfg(test)]
    pub(super) fn current_index(&self) -> usize {
        self.current_index
    }

    #[cfg(test)]
    pub(super) fn current_rotation(&self) -> u8 {
        self.current_slide().rotation
    }

    #[cfg(test)]
    pub(super) fn zoom_mode(&self) -> ImagePreviewZoomMode {
        self.zoom_mode
    }

    #[cfg(test)]
    pub(super) fn oriented_dimensions(&self) -> Option<(u32, u32)> {
        self.current_slide().oriented_pixel_dimensions()
    }

    #[cfg(test)]
    pub(super) fn scroll_offset(&self) -> Point<Pixels> {
        self.scroll_handle.offset()
    }
}

impl Render for ImagePreviewModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let slide = self.current_slide();
        let display_source = slide.display_source();
        let name = slide.item.name.clone();
        let can_reveal = slide.item.can_reveal
            && slide
                .item
                .path
                .as_deref()
                .is_some_and(|path| !path.as_os_str().is_empty());
        let slide_count = self.slides.len();
        let current_index = self.current_index;
        let has_prev = current_index > 0;
        let has_next = current_index + 1 < slide_count;
        let focus = self.focus.clone();
        let scope_focus = self.scope_focus.clone();
        let generation = self.generation;
        let scroll_handle = self.scroll_handle.clone();
        let is_fit = self.zoom_mode == ImagePreviewZoomMode::Fit;
        let processing = slide.processing;
        let saving = self.saving;
        let just_copied = self.copied_generation % 2 == 1;
        let unavailable = slide.is_unavailable();

        // Populate intrinsic dimensions from GPUI's decoded RenderImage without
        // blocking or decoding on the UI thread.
        if !unavailable
            && (slide.pixel_dimensions.get().is_none() || slide.base_render_image.borrow().is_none())
            && let ImagePreviewSource::Image(ref image) = display_source
            && let Some(render_image) = image.clone().get_render_image(window, cx)
        {
            if slide.rotation == 0 && slide.base_render_image.borrow().is_none() {
                *slide.base_render_image.borrow_mut() = Some(render_image.clone());
            }
            let px_size = render_image.size(0);
            if px_size.width.0 > 0 && px_size.height.0 > 0 {
                let (w, h) = (px_size.width.0 as u32, px_size.height.0 as u32);
                let (base_w, base_h) = if slide.rotation % 2 == 1 {
                    (h, w)
                } else {
                    (w, h)
                };
                if slide.pixel_dimensions.get().is_none() {
                    slide.pixel_dimensions.set(Some((base_w, base_h)));
                }
                if slide.logical_dimensions.get().is_none() {
                    slide
                        .logical_dimensions
                        .set(Some((base_w as f32, base_h as f32)));
                }
            }
        }

        let oriented_pixels = slide.oriented_pixel_dimensions();
        let oriented_logical = slide.oriented_logical_dimensions();
        let zoom = self.effective_scale();
        let zoom_label = if is_fit {
            tr!("image_preview.fit_short")
        } else {
            format!("{}%", (zoom * 100.0).round() as u32)
        };
        let rotation_degrees = u16::from(slide.rotation % 4) * 90;

        let (bar_bg, bar_border, bar_shadow, backdrop, text_sm, fg_color, fg_muted) = {
            let ely_theme = cx.theme();
            let colors = &ely_theme.colors;
            (
                colors.overlay,
                colors.border,
                ely_theme.elevation(Elevation::Floating),
                colors.media_backdrop,
                ely_theme.text_size(TextSize::Sm),
                colors.fg,
                colors.fg_muted,
            )
        };

        let top_bar = div()
            .id("image-preview-top-bar")
            .flex()
            .items_center()
            .gap(px(8.0))
            .max_w(px(760.0))
            .px(px(12.0))
            .py(px(6.0))
            .rounded_full()
            .bg(bar_bg)
            .border_1()
            .border_color(bar_border)
            .shadow(bar_shadow.clone())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Icon::new(IconName::Image)
                    .size(ely_gpui_component::theme::IconSize::Sm)
                    .color(fg_muted),
            )
            .child(
                div()
                    .max_w(px(360.0))
                    .text_size(text_sm)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(fg_color)
                    .truncate()
                    .child(name),
            )
            .when_some(oriented_pixels, |bar, (w, h)| {
                bar.child(Badge::new(format!("{w} × {h}")).tone(Tone::Neutral))
            })
            .when(rotation_degrees != 0, |bar| {
                bar.child(Badge::new(format!("{rotation_degrees}°")).tone(Tone::Info))
            })
            .when(slide_count > 1, |bar| {
                bar.child(
                    Badge::new(format!("{} / {slide_count}", current_index + 1))
                        .tone(Tone::Neutral),
                )
            })
            .child(div().h(px(16.0)).w(px(1.0)).bg(bar_border))
            .child(
                div()
                    .debug_selector(|| "image-preview-close".into())
                    .child(
                        IconButton::new("image-preview-close", IconName::X)
                            .size(ControlSize::Sm)
                            .variant(ButtonVariant::Ghost)
                            .tooltip(tr!("attachments.close_preview"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close(window, cx);
                                cx.stop_propagation();
                            })),
                    ),
            );

        let nav_button =
            |id: &'static str, icon_name: IconName, enabled: bool, tip: String, delta: isize| {
                div()
                    .debug_selector(move || id.into())
                    .flex_none()
                    .p(px(4.0))
                    .rounded_full()
                    .bg(bar_bg)
                    .border_1()
                    .border_color(bar_border)
                    .shadow(bar_shadow.clone())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        IconButton::new(id, icon_name)
                            .size(ControlSize::Md)
                            .variant(ButtonVariant::Ghost)
                            .disabled(!enabled)
                            .tooltip(tip)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.step(delta) {
                                    cx.notify();
                                }
                                cx.stop_propagation();
                            })),
                    )
            };

        let unavailable_color = fg_muted;
        let fallback_shadow = bar_shadow.clone();
        let fallback_view = move || {
            div()
                .p(px(24.0))
                .rounded(px(12.0))
                .bg(bar_bg)
                .border_1()
                .border_color(bar_border)
                .shadow(fallback_shadow.clone())
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.0))
                .text_size(ui_px(12.5))
                .text_color(unavailable_color)
                .child(
                    Icon::new(IconName::ImageOff)
                        .size(ely_gpui_component::theme::IconSize::Lg)
                        .color(unavailable_color),
                )
                .child(tr_cow!("attachments.preview_unavailable"))
                .into_any_element()
        };

        let img_id = SharedString::from(format!(
            "image-preview-img-{generation}-{current_index}-{}",
            slide.rotation
        ));
        let make_img = |source: &ImagePreviewSource| match source {
            ImagePreviewSource::Image(image) => img(image.clone()),
            ImagePreviewSource::Render { image, .. } => img(image.clone()),
        };

        let viewport_box = self.viewport_size.get();
        let image_element = if unavailable {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(fallback_view())
                .into_any_element()
        } else if is_fit {
            div()
                .size_full()
                .p(px(CANVAS_PADDING))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("image-preview-surface")
                        .max_w_full()
                        .max_h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                if event.click_count >= 2 && this.toggle_fit_or_actual() {
                                    cx.notify();
                                }
                                cx.stop_propagation();
                            }),
                        )
                        .child(
                            make_img(&display_source)
                                .id(img_id)
                                .max_w(gpui::relative(1.0))
                                .max_h(gpui::relative(1.0))
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(gpui::hsla(0.0, 0.0, 1.0, 0.12))
                                .shadow_lg()
                                .object_fit(ObjectFit::Contain)
                                .with_fallback(fallback_view),
                        ),
                )
                .into_any_element()
        } else {
            let (nat_w, nat_h) = oriented_logical.unwrap_or_else(|| {
                let fallback_w = (f32::from(viewport_box.width) * 0.7).max(240.0);
                let fallback_h = (f32::from(viewport_box.height) * 0.7).max(240.0);
                (fallback_w, fallback_h)
            });
            let display_w = (nat_w * zoom).clamp(16.0, 16384.0);
            let display_h = (nat_h * zoom).clamp(16.0, 16384.0);
            let wrap_w = f32::from(viewport_box.width).max(display_w + CANVAS_PADDING * 2.0);
            let wrap_h = f32::from(viewport_box.height).max(display_h + CANVAS_PADDING * 2.0);
            let overflows = display_w > f32::from(viewport_box.width) - CANVAS_PADDING * 2.0
                || display_h > f32::from(viewport_box.height) - CANVAS_PADDING * 2.0;
            let cursor_style = if self.drag.is_some() {
                CursorStyle::ClosedHand
            } else if overflows {
                CursorStyle::OpenHand
            } else {
                CursorStyle::Arrow
            };

            // Avoid expensive blur shadows and rounded clip shaders on large
            // zoomed canvases so 120Hz+ pan stays GPU-bound free.
            let zoomed_img = make_img(&display_source)
                .id(img_id)
                .size_full()
                .border_1()
                .border_color(gpui::hsla(0.0, 0.0, 1.0, 0.12))
                .object_fit(ObjectFit::Contain)
                .when(!overflows, |el| el.rounded(px(6.0)).shadow_lg())
                .with_fallback(fallback_view);

            div()
                .id("image-preview-scroll")
                .size_full()
                .overflow_scroll()
                .track_scroll(&scroll_handle)
                .when(overflows, |scroll| {
                    scroll.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                })
                .child(
                    div()
                        .flex_none()
                        .w(px(wrap_w))
                        .h(px(wrap_h))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .id("image-preview-surface")
                                .debug_selector(|| "image-preview-surface".into())
                                .flex_none()
                                .w(px(display_w))
                                .h(px(display_h))
                                .cursor(cursor_style)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        if event.click_count >= 2 {
                                            if this.toggle_fit_or_actual() {
                                                cx.notify();
                                            }
                                            cx.stop_propagation();
                                            return;
                                        }
                                        if overflows {
                                            this.drag = Some(DragPanState {
                                                start_mouse: event.position,
                                                start_offset: this.scroll_handle.offset(),
                                            });
                                            cx.notify();
                                        }
                                        cx.stop_propagation();
                                    }),
                                )
                                .child(zoomed_img),
                        ),
                )
                .into_any_element()
        };

        let weak_modal = cx.entity().downgrade();
        let canvas_viewport = self.viewport_size.clone();
        let stage_area = div()
            .relative()
            .flex_1()
            .w_full()
            .min_h_0()
            .child(
                div()
                    .debug_selector(|| "image-preview-stage".into())
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(if slide_count > 1 {
                        SIDE_NAV_CLEARANCE
                    } else {
                        0.0
                    }))
                    .right(px(if slide_count > 1 {
                        SIDE_NAV_CLEARANCE
                    } else {
                        0.0
                    }))
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                        let control = event.modifiers.control || event.modifiers.platform;
                        if !control {
                            return;
                        }
                        let dy = match event.delta {
                            ScrollDelta::Pixels(point) => f32::from(point.y),
                            ScrollDelta::Lines(point) => point.y * 20.0,
                        };
                        if dy == 0.0 {
                            return;
                        }
                        let factor = if dy > 0.0 {
                            WHEEL_ZOOM_STEP
                        } else {
                            1.0 / WHEEL_ZOOM_STEP
                        };
                        if this.zoom(factor) {
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }))
                    .on_pinch(cx.listener(|this, event: &PinchEvent, _, cx| {
                        if event.delta == 0.0 {
                            return;
                        }
                        let factor = (1.0 + event.delta).clamp(0.5, 2.0);
                        if this.zoom(factor) {
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }))
                    .child(image_element)
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                let prev = canvas_viewport.get();
                                let changed = (f32::from(prev.width)
                                    - f32::from(bounds.size.width))
                                .abs()
                                    > 0.5
                                    || (f32::from(prev.height) - f32::from(bounds.size.height))
                                        .abs()
                                        > 0.5;
                                if changed {
                                    let next_size = bounds.size;
                                    let weak = weak_modal.clone();
                                    window.defer(cx, move |_, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.viewport_size.set(next_size);
                                            cx.notify();
                                        });
                                    });
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    ),
            )
            .when(slide_count > 1, |stage| {
                stage
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(12.0))
                            .flex()
                            .items_center()
                            .child(nav_button(
                                "image-preview-prev",
                                IconName::ChevronLeft,
                                has_prev,
                                tr!("image_preview.previous"),
                                -1,
                            )),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right(px(12.0))
                            .flex()
                            .items_center()
                            .child(nav_button(
                                "image-preview-next",
                                IconName::ChevronRight,
                                has_next,
                                tr!("image_preview.next"),
                                1,
                            )),
                    )
            });

        let divider = || div().h(px(16.0)).w(px(1.0)).mx(px(4.0)).bg(bar_border);

        let zoom_toggle_tip = (
            if is_fit {
                tr!("image_preview.actual_size")
            } else {
                tr!("image_preview.fit_to_window")
            },
            if is_fit { "Ctrl+1" } else { "Ctrl+0" },
        );

        let bottom_bar = div()
            .id("image-preview-bottom-bar")
            .flex()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(6.0))
            .rounded_full()
            .bg(bar_bg)
            .border_1()
            .border_color(bar_border)
            .shadow(bar_shadow)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                IconButton::new("image-preview-zoom-out", IconName::ZoomOut)
                    .size(ControlSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .disabled(unavailable || (!is_fit && zoom <= MIN_ZOOM + 0.001))
                    .tooltip(tr!("diagram.zoom_out"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.zoom(1.0 / ZOOM_STEP) {
                            cx.notify();
                        }
                        cx.stop_propagation();
                    })),
            )
            .child(
                div()
                    .id("image-preview-zoom-toggle-wrap")
                    .tooltip(ElyTooltip::with_meta(zoom_toggle_tip.0, zoom_toggle_tip.1))
                    .child(
                        Button::new("image-preview-zoom-toggle", zoom_label)
                            .size(ControlSize::Sm)
                            .variant(ButtonVariant::Ghost)
                            .disabled(unavailable)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.toggle_fit_or_actual() {
                                    cx.notify();
                                }
                                cx.stop_propagation();
                            })),
                    ),
            )
            .child(
                IconButton::new("image-preview-zoom-in", IconName::ZoomIn)
                    .size(ControlSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .disabled(unavailable || (!is_fit && zoom >= MAX_ZOOM - 0.001))
                    .tooltip(tr!("diagram.zoom_in"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.zoom(ZOOM_STEP) {
                            cx.notify();
                        }
                        cx.stop_propagation();
                    })),
            )
            .child(
                IconButton::new("image-preview-fit", IconName::Maximize2)
                    .size(ControlSize::Sm)
                    .variant(if is_fit {
                        ButtonVariant::Subtle
                    } else {
                        ButtonVariant::Ghost
                    })
                    .disabled(unavailable)
                    .tooltip(tr!("image_preview.fit_to_window"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_fit();
                        cx.notify();
                    })),
            )
            .child(divider())
            .child(
                IconButton::new("image-preview-rotate-ccw", IconName::RotateCcw)
                    .size(ControlSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .disabled(unavailable || processing)
                    .tooltip(tr!("image_preview.rotate_left"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.rotate(3, window, cx);
                        cx.stop_propagation();
                    })),
            )
            .child(
                IconButton::new("image-preview-rotate-cw", IconName::RotateCw)
                    .size(ControlSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .disabled(unavailable || processing)
                    .tooltip(tr!("image_preview.rotate_right"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.rotate(1, window, cx);
                        cx.stop_propagation();
                    })),
            )
            .child(divider())
            .child(
                IconButton::new(
                    "image-preview-copy",
                    if just_copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    },
                )
                .size(ControlSize::Sm)
                .variant(ButtonVariant::Ghost)
                .disabled(unavailable || processing)
                .tooltip(tr!("image_preview.copy_image"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.copy_current(cx);
                    cx.stop_propagation();
                })),
            )
            .child(
                IconButton::new("image-preview-save", IconName::Download)
                    .size(ControlSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .disabled(unavailable || saving || processing)
                    .tooltip(tr!("image_preview.save_as"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_current_as(cx);
                        cx.stop_propagation();
                    })),
            )
            .child(
                IconButton::new("image-preview-reveal", IconName::FolderOpen)
                    .size(ControlSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .disabled(!can_reveal)
                    .tooltip(tr!("image_preview.reveal_in_file_manager"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reveal_current(cx);
                        cx.stop_propagation();
                    })),
            );

        let content = div()
            .id(SharedString::from(format!(
                "image-preview-layer-{generation}"
            )))
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .p(px(20.0))
            .track_focus(&focus)
            .key_context(IMAGE_PREVIEW_CONTEXT)
            .on_action(cx.listener(|this, _: &DismissImagePreview, window, cx| {
                this.close(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ImagePreviewPrev, _, cx| {
                if this.step(-1) {
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ImagePreviewNext, _, cx| {
                if this.step(1) {
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ImagePreviewZoomIn, _, cx| {
                if this.zoom(ZOOM_STEP) {
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ImagePreviewZoomOut, _, cx| {
                if this.zoom(1.0 / ZOOM_STEP) {
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ImagePreviewFit, _, cx| {
                this.set_fit();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ImagePreviewActualSize, _, cx| {
                if this.set_actual_size() {
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ImagePreviewRotateCw, window, cx| {
                this.rotate(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ImagePreviewRotateCcw, window, cx| {
                this.rotate(3, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ImagePreviewCopy, _, cx| {
                this.copy_current(cx);
            }))
            .on_action(cx.listener(|this, _: &ImagePreviewSaveAs, _, cx| {
                this.save_current_as(cx);
            }))
            .child(top_bar)
            .child(stage_area)
            .child(bottom_bar);

        let layer = div()
            .id(SharedString::from(format!(
                "image-preview-backdrop-{generation}"
            )))
            .size_full()
            .occlude()
            .bg(backdrop)
            .when(self.drag.is_some(), |el| el.cursor(CursorStyle::ClosedHand))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.close(window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let Some(drag) = this.drag else {
                    return;
                };
                let delta = event.position - drag.start_mouse;
                let max = this.scroll_handle.max_offset();
                let target = point(
                    (drag.start_offset.x + delta.x).clamp(-max.x, px(0.0)),
                    (drag.start_offset.y + delta.y).clamp(-max.y, px(0.0)),
                );
                if target != this.scroll_handle.offset() {
                    this.scroll_handle.set_offset(target);
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if this.drag.take().is_some() {
                        cx.notify();
                        cx.stop_propagation();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if this.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(
                FocusScope::new(&scope_focus)
                    .trap()
                    .size_full()
                    .child(content),
            );

        if cx.reduce_motion() {
            layer.into_any_element()
        } else {
            layer
                .with_animation(
                    SharedString::from(format!("image-preview-enter-{generation}")),
                    Animation::new(IMAGE_PREVIEW_ANIMATION_DURATION).with_easing(ease_out_quint()),
                    |element, delta| element.opacity(delta),
                )
                .into_any_element()
        }
    }
}

pub(super) fn attachment_menu_items(path: PathBuf, can_reveal: bool) -> Vec<MenuItem> {
    let can_reveal = can_reveal && !path.as_os_str().is_empty();
    vec![
        MenuItem::new(tr!("common.reveal_in_finder"), move |_, cx| {
            crate::platform::reveal_in_file_manager(&path, cx);
        })
        .icon("icons/folder.svg")
        .disabled(!can_reveal),
    ]
}

impl Fintwind {
    /// Resolve one daemon-owned image for a visible row. Frames consult only
    /// in-memory state; the first miss starts a deduplicated background RPC and
    /// a later notification lets GPUI render the returned bytes from memory.
    pub(super) fn image_for_reference(
        &self,
        reference: &str,
        daemon_path: Option<&Path>,
        name: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<Arc<gpui::Image>> {
        let attachment_reference =
            reference.starts_with(fintwind_protocol::attachments::ATTACHMENT_SCHEME);
        if !fintwind_protocol::blob::is_reference(reference) && !attachment_reference {
            return None;
        }
        if let Some(state) = self.remote_images.borrow().get(reference) {
            return match state {
                RemoteImageState::Ready(image) => Some(image.clone()),
                RemoteImageState::Loading | RemoteImageState::Unavailable => None,
            };
        }

        let format = name
            .and_then(image_format_for_name)
            .or_else(|| image_format_for_name(reference));

        self.remote_images
            .borrow_mut()
            .insert(reference.to_owned(), RemoteImageState::Loading);
        let cache_key = reference.to_owned();
        let fetch_reference = cache_key.clone();
        let daemon_path = daemon_path.map(Path::to_path_buf);
        let daemon = self.daemon.clone();
        cx.spawn(async move |fintwind, cx| {
            let image = cx
                .background_executor()
                .spawn(async move {
                    let bytes = fintwind_client::persistence::read_remote_reference(
                        &fetch_reference,
                        daemon_path.as_deref(),
                        &daemon,
                    )?;
                    let resolved_format =
                        format.or_else(|| detect_image_format_from_bytes(&bytes))?;
                    Some(Arc::new(gpui::Image::from_bytes(resolved_format, bytes)))
                })
                .await;
            let _ = fintwind.update(cx, |fintwind, cx| {
                fintwind.remote_images.borrow_mut().insert(
                    cache_key,
                    image.map_or(RemoteImageState::Unavailable, RemoteImageState::Ready),
                );
                cx.notify();
            });
        })
        .detach();
        None
    }

    pub(super) fn open_diagram_preview(
        &mut self,
        request: crate::md::diagram::PreviewRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = ImagePreviewItem::from_diagram(request, "Mermaid".into());
        self.open_image_preview_gallery(vec![item], 0, window, cx);
    }

    pub(super) fn open_image_preview_gallery(
        &mut self,
        items: Vec<ImagePreviewItem>,
        initial_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        self.image_preview_generation = self.image_preview_generation.wrapping_add(1);
        let generation = self.image_preview_generation;
        let previous_focus = window.focused(cx);
        let weak_close = cx.entity().downgrade();
        let weak_notify = cx.entity().downgrade();
        let callbacks = ImagePreviewCallbacks {
            on_close: Rc::new(move |previous_focus, window, cx| {
                let _ = weak_close.update(cx, |this, cx| {
                    this.close_image_preview(previous_focus, window, cx);
                });
            }),
            on_notify: Rc::new(move |message, success, cx| {
                let _ = weak_notify.update(cx, |this, cx| {
                    if success {
                        this.show_success_toast(message);
                    } else {
                        this.show_toast(message);
                    }
                    cx.notify();
                });
            }),
            on_reveal: Rc::new(|path, cx| {
                crate::platform::reveal_in_file_manager(path, cx);
            }),
        };

        let modal = cx.new(|cx| {
            ImagePreviewModal::new(
                items,
                initial_index,
                generation,
                previous_focus,
                callbacks,
                cx,
            )
        });
        let focus = modal.read(cx).focus_handle();
        let modal_id = modal.entity_id();
        self.image_preview = Some(modal);

        // The modal sits inside a deferred overlay layer. Focus it two frames
        // later once it has joined the dispatch tree.
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                let mut should_focus = false;
                let _ = weak.update(cx, |this, _| {
                    should_focus = this
                        .image_preview
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

    fn close_image_preview(
        &mut self,
        previous_focus: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.image_preview.take().is_none() {
            return;
        }
        self.image_preview_generation = self.image_preview_generation.wrapping_add(1);
        if let Some(previous) = previous_focus {
            window.focus(&previous, cx);
        } else {
            let focus = self.composer_focus(cx);
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    pub(super) fn render_image_preview(&self) -> Option<AnyElement> {
        let modal = self.image_preview.as_ref()?;
        Some(
            gpui::deferred(div().absolute().inset_0().child(modal.clone()))
                .with_priority(IMAGE_PREVIEW_LAYER_PRIORITY)
                .into_any_element(),
        )
    }
}

fn encode_rotated_png(
    bgra_bytes: &[u8],
    width: u32,
    height: u32,
    quarter_turns: u8,
) -> Option<Vec<u8>> {
    let expected_len = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    if bgra_bytes.len() < expected_len {
        return None;
    }
    let mut rgba = bgra_bytes[..expected_len].to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba)?;
    let rotated = match quarter_turns % 4 {
        1 => image::imageops::rotate90(&buffer),
        2 => image::imageops::rotate180(&buffer),
        3 => image::imageops::rotate270(&buffer),
        _ => buffer,
    };
    let mut out = Vec::new();
    rotated
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

fn default_save_directory() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::picture_dir)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn suggested_save_filename(
    name: &str,
    format: gpui::ImageFormat,
    reencoded_png: bool,
) -> String {
    let trimmed = name.trim();
    let base_name = if trimmed.is_empty() { "image" } else { trimmed };
    let ext = if reencoded_png {
        "png"
    } else {
        extension_for_image_format(format)
    };
    let path = Path::new(base_name);
    if reencoded_png {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("image");
        return format!("{stem}.{ext}");
    }
    if path.extension().is_some() {
        base_name.to_owned()
    } else {
        format!("{base_name}.{ext}")
    }
}

pub(super) fn extension_for_image_format(format: gpui::ImageFormat) -> &'static str {
    match format {
        gpui::ImageFormat::Png => "png",
        gpui::ImageFormat::Jpeg => "jpg",
        gpui::ImageFormat::Webp => "webp",
        gpui::ImageFormat::Gif => "gif",
        gpui::ImageFormat::Svg => "svg",
        gpui::ImageFormat::Bmp => "bmp",
        gpui::ImageFormat::Tiff => "tiff",
        gpui::ImageFormat::Ico => "ico",
        gpui::ImageFormat::Pnm => "pnm",
    }
}

fn detect_image_format_from_bytes(bytes: &[u8]) -> Option<gpui::ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(gpui::ImageFormat::Png)
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some(gpui::ImageFormat::Jpeg)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(gpui::ImageFormat::Webp)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(gpui::ImageFormat::Gif)
    } else if bytes.starts_with(b"BM") {
        Some(gpui::ImageFormat::Bmp)
    } else if bytes.starts_with(b"<svg") || bytes.windows(4).any(|w| w == b"<svg") {
        Some(gpui::ImageFormat::Svg)
    } else {
        None
    }
}

pub(super) fn image_format_for_name(name: &str) -> Option<gpui::ImageFormat> {
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(gpui::ImageFormat::Png),
        "jpg" | "jpeg" => Some(gpui::ImageFormat::Jpeg),
        "webp" => Some(gpui::ImageFormat::Webp),
        "gif" => Some(gpui::ImageFormat::Gif),
        "svg" => Some(gpui::ImageFormat::Svg),
        "bmp" => Some(gpui::ImageFormat::Bmp),
        "tif" | "tiff" => Some(gpui::ImageFormat::Tiff),
        "ico" => Some(gpui::ImageFormat::Ico),
        "pnm" | "pbm" | "pgm" | "ppm" => Some(gpui::ImageFormat::Pnm),
        _ => None,
    }
}
