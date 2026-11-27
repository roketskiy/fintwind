//! Fintwind host adapter for [`fintwind_preview_ui::ImagePreviewModal`].

use std::rc::Rc;

use gpui::FocusHandle;

use super::*;

pub use fintwind_preview_ui::*;

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
