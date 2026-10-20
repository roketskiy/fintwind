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
                weekdays: Weekdays::every_day().bits(),
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

    pub(super) fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
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
        .size(ControlSize::Sm)
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
                .size(ControlSize::Sm)
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
                .child(
                    div()
                        .w(px(90.0))
                        .child(Input::new(&self.interval_input).size(ControlSize::Sm)),
                )
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
            .items_center()
            .gap_3()
            .child(schedule)
            .child(value);
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
            ))
            .child(field(tr!("scheduled_tasks.schedule_label"), timing));
        if editor.schedule == ScheduleKind::AtTime {
            form = form.child(div().flex().items_center().gap_1().children(
                WEEKDAY_ORDER.iter().map(|&(bit, key)| {
                    let selected = editor.weekdays & (1 << bit) != 0;
                    let weak = cx.entity().downgrade();
                    ToggleButton::new(
                        SharedString::from(format!("scheduled-weekday-{bit}")),
                        ToggleItem::new(bit.to_string()).label(tr!(key)),
                        selected,
                    )
                    .size(ControlSize::Sm)
                    .disabled(busy)
                    .on_toggle(move |enabled, _, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.edit(
                                |draft| {
                                    if enabled {
                                        draft.weekdays |= 1 << bit;
                                    } else {
                                        draft.weekdays &= !(1 << bit);
                                    }
                                },
                                cx,
                            )
                        });
                    })
                }),
            ));
        }
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
        .size(ControlSize::Sm)
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
        form = form.child(field(tr!("scheduled_tasks.target_label"), target));
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
                .child(field(tr!("scheduled_tasks.project_label"), project))
                .child(field(tr!("scheduled_tasks.model_label"), model))
                .child(field(tr!("scheduled_tasks.access_label"), access));
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
            form = form.child(field(tr!("scheduled_tasks.session_label"), session));
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
