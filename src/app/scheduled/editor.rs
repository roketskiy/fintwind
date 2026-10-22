use super::*;

impl ScheduledTasksView {
    fn editor(&self) -> Option<&TaskEditor> {
        match self.dialog.as_ref()? {
            TaskDialog::Edit(editor) | TaskDialog::Discard(editor) => Some(editor),
            TaskDialog::Delete { .. } => None,
        }
    }

    fn edit(&mut self, change: impl FnOnce(&mut Editor), cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        if let Some(TaskDialog::Edit(editor)) = &mut self.dialog {
            change(&mut editor.draft);
            self.refresh_picker_choices(cx);
            cx.notify();
        }
    }

    fn read_editor(&self, cx: &App) -> Option<Editor> {
        let mut draft = self.editor()?.draft.clone();
        draft.title = self.title_input.read(cx).text().to_owned();
        draft.prompt = self.prompt_input.read(cx).text().to_owned();
        draft.interval_minutes = self.interval_input.read(cx).text().to_owned();
        Some(draft)
    }

    fn show_editor(&mut self, draft: Editor, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.previous_focus = window.focused(cx);
        self.title_input
            .update(cx, |input, cx| input.set_text(draft.title.clone(), cx));
        self.prompt_input
            .update(cx, |input, cx| input.set_text(draft.prompt.clone(), cx));
        self.interval_input.update(cx, |input, cx| {
            input.set_text(draft.interval_minutes.clone(), cx)
        });
        self.model_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.session_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.dialog = Some(TaskDialog::Edit(TaskEditor {
            original: draft.clone(),
            draft,
        }));
        self.refresh_picker_choices(cx);
        self.error = None;
        if self
            .editor()
            .is_some_and(|editor| editor.draft.task_id.is_none())
        {
            window.focus(&self.dialog_focus, cx);
        }
        let focus = self.title_input.read(cx).focus_handle(cx);
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| window.focus(&focus, cx))
        });
        cx.notify();
    }

    pub(super) fn open_task(
        &mut self,
        id: ScheduledTaskId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(task) = self.task(id) {
            self.show_editor(Editor::for_task(task), window, cx);
        }
    }

    pub(super) fn open_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let launch = self
            .default_launch
            .clone()
            .unwrap_or_else(|| SessionLaunch {
                model: String::new(),
                runtime_mode: RuntimeMode::Ask,
                interaction_mode: InteractionMode::default(),
                reasoning_effort: None,
                service_tier: None,
                context_window: None,
                agent_preset: None,
            });
        self.show_editor(
            Editor {
                task_id: None,
                revision: 0,
                enabled: true,
                title: String::new(),
                prompt: String::new(),
                schedule: ScheduleKind::AtTime,
                time: "09:00".into(),
                weekdays: 0,
                interval_minutes: "60".into(),
                target: TargetKind::NewSession,
                project_id: self.projects.first().map(|project| project.id),
                session_id: None,
                launch,
                workspace: SessionWorkspace::Local,
            },
            window,
            cx,
        );
    }

    pub(super) fn finish_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialog = None;
        if let Some(focus) = self.previous_focus.take() {
            let window = self.window;
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, cx| window.focus(&focus, cx));
            });
        }
        cx.notify();
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            window.focus(&self.confirm_focus, cx);
            return;
        }
        let changed = self
            .read_editor(cx)
            .zip(self.editor())
            .is_some_and(|(draft, editor)| draft != editor.original);
        if changed {
            if let Some(TaskDialog::Edit(editor)) = self.dialog.take() {
                self.dialog = Some(TaskDialog::Discard(editor));
            }
            cx.notify();
        } else {
            self.error = None;
            self.finish_dialog(cx);
        }
    }

    fn resume_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(TaskDialog::Discard(editor)) = self.dialog.take() {
            self.dialog = Some(TaskDialog::Edit(editor));
        }
        let focus = self.title_input.read(cx).focus_handle(cx);
        window.on_next_frame(move |window, cx| window.focus(&focus, cx));
        cx.notify();
    }

    pub(super) fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy() || !matches!(self.dialog, Some(TaskDialog::Edit(_))) {
            return;
        }
        let Some(editor) = self.read_editor(cx) else {
            return;
        };
        let draft = match editor.draft() {
            Ok(draft) => draft,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        };
        let edit = match editor.task_id {
            Some(id) => ScheduledTaskEdit::Update {
                id,
                expected_revision: editor.revision,
                draft,
            },
            None => ScheduledTaskEdit::Create { draft },
        };
        self.send(ScheduledRequest::Save(edit), Mutation::SaveEditor, cx);
    }

    pub(super) fn request_delete(
        &mut self,
        id: ScheduledTaskId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy() {
            return;
        }
        let Some(task) = self.task(id) else {
            return;
        };
        self.dialog = Some(TaskDialog::Delete {
            id,
            revision: task.revision,
            title: task.title.clone(),
        });
        self.previous_focus = window.focused(cx);
        self.error = None;
        cx.notify();
    }

    pub(super) fn render_dialog(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.dialog.as_ref()?;
        let weak = cx.entity().downgrade();
        let busy = self.busy();
        let theme = Theme::current(cx);
        let dialog = match state {
            TaskDialog::Edit(editor) => {
                let cancel = command_button(
                    "scheduled-editor-cancel",
                    tr!("scheduled_tasks.cancel"),
                    ButtonVariant::Outline,
                    !busy,
                    cx,
                    |this, window, cx| this.request_close(window, cx),
                );
                let save = command_button(
                    "scheduled-editor-save",
                    tr!("scheduled_tasks.save"),
                    ButtonVariant::Primary,
                    true,
                    cx,
                    |this, _, cx| this.save(cx),
                )
                .loading(busy)
                .focus_handle(&self.confirm_focus);
                let feedback = div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui_px(12.0))
                    .text_color(theme.danger_text)
                    .children(self.error.clone());
                if editor.draft.task_id.is_none() {
                    use ely_gpui_component::{
                        primitives::{FocusScope, raise},
                        theme::{ActiveTheme, Elevation, Radius, TextSize},
                    };
                    // Ely's Dialog and Backdrop always animate their entrance.
                    // Keep the same card and focus scope, but show both layers immediately.
                    let ely = cx.theme();
                    let backdrop = ely.colors.backdrop;
                    let margin = ely.titlebar_height().to_pixels(window.rem_size());
                    let card = div()
                        .id("scheduled-editor")
                        .role(gpui::Role::Dialog)
                        .aria_label(tr!("scheduled_tasks.new"))
                        .flex()
                        .flex_col()
                        .gap_5()
                        .w(ely.dialog_width())
                        .max_w(window.viewport_size().width - px(32.0))
                        .max_h(window.viewport_size().height - margin * 2.0)
                        .p_6()
                        .rounded(ely.radius(Radius::Xl))
                        .bg(ely.colors.overlay)
                        .border_1()
                        .border_color(ely.colors.border)
                        .shadow(ely.elevation(Elevation::Modal))
                        .child(
                            div()
                                .text_size(ely.text_size(TextSize::Lg))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ely.colors.fg)
                                .child(tr!("scheduled_tasks.new")),
                        )
                        .child(
                            div()
                                .id("scheduled-editor-body")
                                .min_h_0()
                                .overflow_y_scroll()
                                .child(self.render_form(&editor.draft, &theme, cx)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(feedback)
                                .child(cancel)
                                .child(save),
                        );
                    return Some(
                        raise(
                            "scheduled-editor-static",
                            gpui::anchored().position(gpui::Point::default()).child(
                                div()
                                    .relative()
                                    .w(window.viewport_size().width)
                                    .h(window.viewport_size().height)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .bg(backdrop)
                                            .occlude()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, window, cx| {
                                                    this.request_close(window, cx);
                                                }),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .relative()
                                            .occlude()
                                            .on_key_down(cx.listener(
                                                |this, event: &KeyDownEvent, window, cx| {
                                                    if event.keystroke.key == "escape" {
                                                        cx.stop_propagation();
                                                        this.request_close(window, cx);
                                                    }
                                                },
                                            ))
                                            .child(
                                                FocusScope::new(&self.dialog_focus)
                                                    .trap()
                                                    .child(card),
                                            ),
                                    ),
                            ),
                        )
                        .into_any_element(),
                    );
                }
                Dialog::new(
                    "scheduled-editor",
                    tr!(if editor.draft.task_id.is_some() {
                        "scheduled_tasks.edit"
                    } else {
                        "scheduled_tasks.new"
                    }),
                    move |window, cx| {
                        let _ = weak.update(cx, |this, cx| this.request_close(window, cx));
                    },
                )
                .child(self.render_form(&editor.draft, &theme, cx))
                .action(move |_| feedback)
                .action(move |_| cancel)
                .action(move |_| save)
            }
            TaskDialog::Discard(_) => {
                let keep = command_button(
                    "scheduled-keep-editing",
                    tr!("scheduled_tasks.keep_editing"),
                    ButtonVariant::Outline,
                    true,
                    cx,
                    |this, window, cx| this.resume_edit(window, cx),
                );
                let discard = command_button(
                    "scheduled-discard",
                    tr!("scheduled_tasks.discard"),
                    ButtonVariant::Danger,
                    true,
                    cx,
                    |this, _, cx| {
                        this.error = None;
                        this.finish_dialog(cx);
                    },
                );
                Dialog::new(
                    "scheduled-discard-dialog",
                    tr!("scheduled_tasks.discard_title"),
                    move |window, cx| {
                        let _ = weak.update(cx, |this, cx| this.resume_edit(window, cx));
                    },
                )
                .action(move |_| keep)
                .action(move |_| discard)
            }
            TaskDialog::Delete {
                id,
                revision,
                title,
            } => {
                let (id, revision) = (*id, *revision);
                let cancel = command_button(
                    "scheduled-delete-cancel",
                    tr!("scheduled_tasks.cancel"),
                    ButtonVariant::Outline,
                    !busy,
                    cx,
                    |this, window, cx| this.request_close(window, cx),
                );
                let delete = command_button(
                    "scheduled-delete-confirm",
                    tr!("scheduled_tasks.delete"),
                    ButtonVariant::Danger,
                    true,
                    cx,
                    move |this, _, cx| {
                        this.send(
                            ScheduledRequest::Delete {
                                id,
                                expected_revision: revision,
                            },
                            Mutation::Delete(id),
                            cx,
                        )
                    },
                )
                .loading(busy)
                .focus_handle(&self.confirm_focus);
                Dialog::new(
                    "scheduled-delete-dialog",
                    tr!("scheduled_tasks.delete_confirm", title = title.clone()),
                    move |window, cx| {
                        let _ = weak.update(cx, |this, cx| this.request_close(window, cx));
                    },
                )
                .child(
                    div()
                        .text_size(ui_px(12.0))
                        .text_color(theme.text_secondary)
                        .child(tr!("scheduled_tasks.delete_body")),
                )
                .children(
                    self.error
                        .as_ref()
                        .map(|error| self.render_error(error, cx)),
                )
                .action(move |_| cancel)
                .action(move |_| delete)
            }
        };
        Some(dialog.into_any_element())
    }

    fn render_form(&self, editor: &Editor, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let busy = self.busy();
        let schedule = Select::new(
            "scheduled-schedule-kind",
            [
                Choice::new("fixed", tr!("scheduled_tasks.mode_at_time")),
                Choice::new("interval", tr!("scheduled_tasks.mode_interval")),
            ],
        )
        .selected(if editor.schedule == ScheduleKind::AtTime {
            "fixed"
        } else {
            "interval"
        })
        .disabled(busy)
        .on_change(cx.listener(|this, value: &SharedString, _, cx| {
            this.edit(
                |draft| {
                    draft.schedule = if value.as_ref() == "fixed" {
                        ScheduleKind::AtTime
                    } else {
                        ScheduleKind::Every
                    }
                },
                cx,
            )
        }));
        let weak = cx.entity().downgrade();
        let value = if editor.schedule == ScheduleKind::AtTime {
            TimePicker::new("scheduled-time", editor.time.parse().ok())
                .step(1)
                .disabled(busy)
                .on_change(move |time, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.edit(
                            |draft| draft.time = format!("{:02}:{:02}", time.hour(), time.minute()),
                            cx,
                        )
                    });
                })
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(90.0)).child(Input::new(&self.interval_input)))
                .child(
                    div()
                        .text_size(ui_px(12.0))
                        .text_color(theme.text_secondary)
                        .child(tr!("scheduled_tasks.interval_unit")),
                )
                .into_any_element()
        };
        let timing = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(schedule)
            .child(value);
        let mut timing = div().flex().flex_col().gap_3().child(timing);
        let mut form = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(field(
                tr!("scheduled_tasks.name_label"),
                Input::new(&self.title_input),
            ))
            .child(field(
                tr!("scheduled_tasks.prompt_label"),
                Input::new(&self.prompt_input),
            ));
        if editor.schedule == ScheduleKind::AtTime {
            let choices = WEEKDAY_ORDER.iter().map(|&(bit, key)| {
                let mut choice = Choice::new(bit.to_string(), tr!(key));
                if busy {
                    choice.disabled = true;
                }
                choice
            });
            let selected = WEEKDAY_ORDER
                .iter()
                .filter(|&&(bit, _)| editor.weekdays & (1 << bit) != 0)
                .map(|&(bit, _)| SharedString::from(bit.to_string()));
            let weak = cx.entity().downgrade();
            let weekdays = MultiSelect::new("scheduled-weekdays", choices)
                .selected(selected)
                .placeholder(tr!("scheduled_tasks.repeat_label"))
                .on_change(move |values, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.edit(
                            |draft| {
                                let mut bits = 0u8;
                                for value in values {
                                    if let Ok(bit) = value.parse::<u8>() {
                                        bits |= 1 << bit;
                                    }
                                }
                                draft.weekdays = bits;
                            },
                            cx,
                        )
                    });
                });
            timing = timing.child(weekdays);
        }
        form = form.child(field(tr!("scheduled_tasks.schedule_label"), timing));
        let target = Select::new(
            "scheduled-target-kind",
            [
                Choice::new("new", tr!("scheduled_tasks.target_new")),
                Choice::new("existing", tr!("scheduled_tasks.target_existing")),
            ],
        )
        .selected(if editor.target == TargetKind::NewSession {
            "new"
        } else {
            "existing"
        })
        .disabled(busy)
        .on_change(cx.listener(|this, value: &SharedString, _, cx| {
            this.edit(
                |draft| {
                    draft.target = if value.as_ref() == "new" {
                        TargetKind::NewSession
                    } else {
                        TargetKind::ExistingSession
                    }
                },
                cx,
            )
        }));
        let target = field(tr!("scheduled_tasks.target_label"), target);
        if editor.target == TargetKind::NewSession {
            let project = Select::new(
                "scheduled-project",
                self.projects
                    .iter()
                    .map(|p| Choice::new(p.id.to_string(), p.name.clone())),
            )
            .selected(
                editor
                    .project_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            )
            .placeholder(tr!("scheduled_tasks.project_placeholder"))
            .disabled(busy || self.projects.is_empty())
            .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                if let Ok(id) = Uuid::parse_str(value) {
                    this.edit(
                        |draft| {
                            if draft.project_id != Some(id) {
                                draft.project_id = Some(id);
                                draft.workspace = SessionWorkspace::Local;
                            }
                        },
                        cx,
                    );
                }
            }));
            let model = Combobox::new(
                "scheduled-model",
                &self.model_input,
                self.shown_models.clone(),
            )
            .selected(editor.launch.model.clone())
            .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                this.edit(
                    |draft| {
                        if draft.launch.model != value.as_ref() {
                            draft.launch.model = value.to_string();
                            draft.launch.reasoning_effort = None;
                            draft.launch.service_tier = None;
                            draft.launch.context_window = None;
                        }
                    },
                    cx,
                )
            }));
            let access = Select::new(
                "scheduled-access",
                RuntimeMode::ACCESS_OPTIONS
                    .into_iter()
                    .enumerate()
                    .map(|(index, mode)| Choice::new(index.to_string(), mode.label())),
            )
            .selected(
                RuntimeMode::ACCESS_OPTIONS
                    .iter()
                    .position(|mode| *mode == editor.launch.runtime_mode.access())
                    .unwrap_or(0)
                    .to_string(),
            )
            .disabled(busy)
            .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                if let Some(mode) = value
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| RuntimeMode::ACCESS_OPTIONS.get(i))
                    .copied()
                {
                    this.edit(|draft| draft.launch.runtime_mode = mode, cx);
                }
            }));
            form = form
                .child(field_pair(
                    target,
                    field(tr!("scheduled_tasks.project_label"), project),
                ))
                .child(field_pair(
                    field(tr!("scheduled_tasks.model_label"), model),
                    field(tr!("scheduled_tasks.access_label"), access),
                ));
        } else {
            let session = Combobox::new(
                "scheduled-session",
                &self.session_input,
                self.shown_sessions.clone(),
            )
            .selected(
                editor
                    .session_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            )
            .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                if let Ok(id) = Uuid::parse_str(value) {
                    this.edit(|draft| draft.session_id = Some(id), cx);
                }
            }));
            form = form
                .child(target)
                .child(field(tr!("scheduled_tasks.session_label"), session));
        }
        let weak = cx.entity().downgrade();
        form.child(
            ely_gpui_component::forms::Switch::new("scheduled-editor-enabled", editor.enabled)
                .label(tr!("scheduled_tasks.enabled"))
                .disabled(busy)
                .on_change(move |enabled, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.edit(|draft| draft.enabled = enabled, cx)
                    });
                }),
        )
    }
}

fn field(label: String, control: impl IntoElement) -> Div {
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(ui_px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
        .child(control)
}

fn field_pair(first: Div, second: Div) -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap_4()
        .child(first.flex_1().min_w(px(180.0)))
        .child(second.flex_1().min_w(px(180.0)))
}
