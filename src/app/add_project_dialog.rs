use std::path::PathBuf;

use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    theme::ControlSize,
};
use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, MouseButton,
    ParentElement, PathBuilder, PathPromptOptions, Styled, Window, canvas, div, point, prelude::*,
    px,
};

use crate::input::{ComposerEvent, ComposerInput};
use crate::model::Project;
use crate::theme::{Theme, ui_px};
use crate::ui::text_field::TextField;
use crate::ui::{icon, icon_button};

use super::Fintwind;

/// The add-project form: a fixed local location, a dashed folder drop area
/// and an optional display name. Icon, color and index pickers from the
/// reference are intentionally absent.
pub(super) struct AddProjectDialogState {
    folder: Option<PathBuf>,
    name: Entity<ComposerInput>,
    folder_focus: FocusHandle,
}

impl Fintwind {
    pub(super) fn open_add_project_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("project.name_placeholder"))
        });
        cx.subscribe(
            &name,
            |this: &mut Self, _, event: &ComposerEvent, cx| {
                if matches!(event, ComposerEvent::Submit(_)) {
                    this.confirm_add_project(cx);
                }
            },
        )
        .detach();
        let folder_focus = cx.focus_handle();
        self.add_project_dialog = Some(AddProjectDialogState {
            folder: None,
            name,
            folder_focus: folder_focus.clone(),
        });
        // The deferred layer joins the dispatch tree only after it draws;
        // focus the folder area two frames later so keys cannot fall through.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| window.focus(&folder_focus, cx));
        });
        cx.notify();
    }

    pub(super) fn close_add_project_dialog(&mut self, cx: &mut Context<Self>) {
        self.add_project_dialog = None;
        cx.notify();
    }

    fn pick_add_project_folder(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr!("project.add_project").into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| {
                    let Some(dialog) = this.add_project_dialog.as_mut() else {
                        return;
                    };
                    let previous_default = dialog
                        .folder
                        .as_ref()
                        .and_then(|folder| folder.file_name())
                        .and_then(|name| name.to_str())
                        .map(str::to_owned);
                    let current = dialog.name.read(cx).content().trim().to_owned();
                    let next_default = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Project")
                        .to_owned();
                    // Prefill the folder name, but never overwrite a name the
                    // user typed themselves.
                    if current.is_empty() || Some(current) == previous_default {
                        dialog
                            .name
                            .update(cx, |input, cx| input.set_content(next_default, cx));
                    }
                    dialog.folder = Some(path);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn confirm_add_project(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.add_project_dialog.as_ref() else {
            return;
        };
        let Some(path) = dialog.folder.clone() else {
            return;
        };
        let name = dialog.name.read(cx).content().trim().to_owned();
        self.add_project_dialog = None;
        if let Some(existing) = self.state.projects.iter().find(|project| project.path == path) {
            self.select_project(existing.id, cx);
        } else {
            let mut project = Project::from_path(path);
            if !name.is_empty() {
                project.name = name;
            }
            let project_id = project.id;
            self.state.projects.push(project);
            self.create_session_for(project_id, cx);
        }
        cx.notify();
    }

    pub(super) fn render_add_project_dialog(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.add_project_dialog.as_ref()?;
        let theme = Theme::current(cx);
        let folder = dialog.folder.clone();
        let name = dialog.name.clone();
        let folder_focus = dialog.folder_focus.clone();
        let weak = cx.entity().downgrade();
        let can_confirm = folder.is_some();

        let label = |text: String| {
            div()
                .text_size(ui_px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text_secondary)
                .child(text)
        };

        let pick_weak = weak.clone();
        let key_weak = weak.clone();
        let folder_label = folder
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| tr!("project.folder_pick"));
        let folder_area = div()
            .id("add-project-folder")
            .track_focus(&folder_focus)
            .tab_index(0)
            .relative()
            .h(px(120.0))
            .w_full()
            .rounded(px(10.0))
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .hover(|style| style.bg(theme.overlay))
            .on_click(move |_, _, cx| {
                let _ = pick_weak.update(cx, |this, cx| this.pick_add_project_folder(cx));
            })
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    let _ = key_weak.update(cx, |this, cx| this.pick_add_project_folder(cx));
                    cx.stop_propagation();
                }
            })
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let x = bounds.origin.x + px(0.5);
                        let y = bounds.origin.y + px(0.5);
                        let w = bounds.size.width - px(1.0);
                        let h = bounds.size.height - px(1.0);
                        let r = px(10.0);
                        let mut builder =
                            PathBuilder::stroke(px(1.0)).dash_array(&[px(4.0), px(4.0)]);
                        builder.move_to(point(x + r, y));
                        builder.line_to(point(x + w - r, y));
                        builder.arc_to(point(r, r), px(0.0), false, true, point(x + w, y + r));
                        builder.line_to(point(x + w, y + h - r));
                        builder.arc_to(
                            point(r, r),
                            px(0.0),
                            false,
                            true,
                            point(x + w - r, y + h),
                        );
                        builder.line_to(point(x + r, y + h));
                        builder.arc_to(point(r, r), px(0.0), false, true, point(x, y + h - r));
                        builder.line_to(point(x, y + r));
                        builder.arc_to(point(r, r), px(0.0), false, true, point(x + r, y));
                        builder.close();
                        if let Ok(border) = builder.build() {
                            window.paint_path(border, theme.border_strong);
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .child(icon(
                        if folder.is_some() {
                            "icons/folder.svg"
                        } else {
                            "icons/folder-new.svg"
                        },
                        16.0,
                        theme.text_tertiary,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(12.5))
                            .text_color(if folder.is_some() {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .child(folder_label),
                    ),
            );

        let close_weak = weak.clone();
        let cancel_weak = weak.clone();
        let confirm_weak = weak;
        let card = div()
            .id("add-project-dialog-card")
            .w_full()
            .max_w(px(520.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .shadow_lg()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key.as_str() == "escape" {
                    this.close_add_project_dialog(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .h(px(48.0))
                    .px(px(16.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(ui_px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(tr!("project.add_project")),
                    )
                    .child(
                        icon_button("add-project-close", "icons/x.svg", theme).on_click(
                            move |_, _, cx| {
                                let _ = close_weak
                                    .update(cx, |this, cx| this.close_add_project_dialog(cx));
                            },
                        ),
                    ),
            )
            .child(
                div()
                    .px(px(20.0))
                    .pb(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(label(tr!("project.location_label")))
                            .child(
                                div()
                                    .h(px(36.0))
                                    .px(px(10.0))
                                    .rounded(px(8.0))
                                    .border_1()
                                    .border_color(theme.border_strong)
                                    .bg(theme.composer)
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(icon("icons/laptop.svg", 14.0, theme.text_tertiary))
                                    .child(
                                        div()
                                            .text_size(ui_px(12.5))
                                            .text_color(theme.text)
                                            .child(tr!("project.location_local")),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(label(tr!("project.folder_label")))
                            .child(folder_area),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(label(tr!("project.name_label")))
                            .child(TextField::new("add-project-name", name).w_full()),
                    ),
            )
            .child(
                div()
                    .px(px(16.0))
                    .py(px(12.0))
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        Button::new("add-project-cancel", tr!("common.cancel"))
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let _ = cancel_weak
                                    .update(cx, |this, cx| this.close_add_project_dialog(cx));
                            }),
                    )
                    .child(
                        Button::new("add-project-confirm", tr!("project.add_confirm"))
                            .variant(ButtonVariant::Primary)
                            .size(ControlSize::Sm)
                            .disabled(!can_confirm)
                            .on_click(move |_, _, cx| {
                                let _ = confirm_weak
                                    .update(cx, |this, cx| this.confirm_add_project(cx));
                            }),
                    ),
            );

        let scrim = if theme.is_dark {
            gpui::hsla(0.0, 0.0, 0.0, 0.34)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.16)
        };
        let layer = div()
            .id("add-project-dialog-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(scrim)
            .p(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_add_project_dialog(cx)),
            )
            .child(card);
        Some(gpui::deferred(layer).with_priority(4).into_any_element())
    }
}
