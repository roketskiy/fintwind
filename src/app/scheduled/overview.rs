use ely_gpui_component::menus::{Menu, MenuItem, OverflowMenu};

use super::*;

impl ScheduledTasksView {
    pub(super) fn render_error(&self, message: &SharedString, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(theme.danger_soft)
            .child(icon("icons/alert.svg", 14.0, theme.danger_text))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui_px(12.0))
                    .text_color(theme.danger_text)
                    .child(message.clone()),
            )
            .when(self.dialog.is_none(), |row| {
                row.child(command_button(
                    "scheduled-retry",
                    tr!("scheduled_tasks.retry"),
                    ButtonVariant::Ghost,
                    !self.busy(),
                    cx,
                    |this, _, cx| this.retry(cx),
                ))
            })
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let filter = SegmentedControl::new(
            "scheduled-filter",
            match self.filter {
                TaskFilter::All => "all",
                TaskFilter::Active => "active",
                TaskFilter::Paused => "paused",
            },
        )
        .segment("all", tr!("scheduled_tasks.all"), None)
        .segment("active", tr!("scheduled_tasks.active"), None)
        .segment("paused", tr!("scheduled_tasks.paused"), None)
        .size(ControlSize::Sm)
        .on_change(cx.listener(|this, value: &SharedString, _, cx| {
            this.set_filter(
                match value.as_ref() {
                    "active" => TaskFilter::Active,
                    "paused" => TaskFilter::Paused,
                    _ => TaskFilter::All,
                },
                cx,
            )
        }));
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_5()
            .pb_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .text_size(ui_px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(tr!("settings.scheduled_tasks")),
                    )
                    .child(
                        command_button(
                            "scheduled-new-task",
                            tr!("scheduled_tasks.new"),
                            ButtonVariant::Primary,
                            !self.busy(),
                            cx,
                            |this, window, cx| this.open_new_task(window, cx),
                        )
                        .icon(IconName::Plus),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(filter)
                    .child(div().flex_1())
                    .child(
                        div().w(px(240.0)).child(
                            Input::new(&self.search_input)
                                .size(ControlSize::Sm)
                                .clearable(),
                        ),
                    ),
            )
    }

    fn render_task_row(
        &self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(&task_index) = self.visible.get(index) else {
            return div().into_any_element();
        };
        let Some(task) = self.tasks.get(task_index) else {
            return div().into_any_element();
        };
        let theme = Theme::current(cx);
        let id = task.id;
        let enabled = task.enabled;
        let busy = self.busy();
        let weak = cx.entity().downgrade();
        let edit = MenuItem::new(tr!("scheduled_tasks.edit"))
            .icon(IconName::Pencil)
            .disabled(busy)
            .on_click(move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.open_task(id, window, cx));
            });
        let weak = cx.entity().downgrade();
        let run = MenuItem::new(tr!("scheduled_tasks.run_now"))
            .icon(IconName::Play)
            .disabled(busy || task.pending.is_some())
            .on_click(move |_, cx| {
                let _ = weak.update(cx, |this, cx| this.run_now(id, cx));
            });
        let weak = cx.entity().downgrade();
        let delete = MenuItem::new(tr!("scheduled_tasks.delete"))
            .icon(IconName::Trash2)
            .disabled(busy)
            .on_click(move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.request_delete(id, window, cx));
            });
        let menu = Menu::new().item(edit).item(run).separator().item(delete);
        let state = TaskState::of(task);
        let (status, color) = if !enabled {
            (tr!("scheduled_tasks.paused"), theme.text_tertiary)
        } else {
            let color = match state {
                TaskState::Rejected | TaskState::NotSent | TaskState::Unconfirmed => {
                    theme.danger_text
                }
                _ => theme.text_tertiary,
            };
            (state.label(), color)
        };
        let target = &self.task_targets[task_index];
        let mut metadata = format!(
            "{target} · {} · {}",
            schedule_summary(task.schedule),
            if enabled {
                next_due_label(task)
            } else {
                tr!("scheduled_tasks.next_run_none")
            }
        );
        if let Some(last) = task.last.as_ref() {
            metadata.push_str(&format!(
                " · {}",
                tr!("scheduled_tasks.last_run", time = format_timestamp(last.at))
            ));
        }
        let result_session = last_session(task).filter(|id| self.session_ids.contains(id));
        div()
            .id(SharedString::from(format!("scheduled-task-{}", id.0)))
            .h(self.row_height)
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_4()
            .border_b_1()
            .border_color(theme.border)
            .px_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(command_button(
                                SharedString::from(format!("scheduled-task-title-{}", id.0)),
                                task.title.clone(),
                                ButtonVariant::Ghost,
                                !busy,
                                cx,
                                move |this, window, cx| this.open_task(id, window, cx),
                            ))
                            .child(div().text_size(ui_px(11.0)).text_color(color).child(status)),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(ui_px(12.0))
                            .text_color(theme.text_secondary)
                            .child(task.prompt.lines().next().unwrap_or_default().to_owned()),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(ui_px(11.0))
                            .text_color(theme.text_tertiary)
                            .child(metadata),
                    )
                    .when_some(outcome_reason(task), |column, reason| {
                        column.child(
                            div()
                                .w_full()
                                .truncate()
                                .text_size(ui_px(11.0))
                                .text_color(theme.danger_text)
                                .child(reason),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when_some(result_session, |row, session_id| {
                        row.child(
                            command_button(
                                SharedString::from(format!("scheduled-task-result-{}", id.0)),
                                tr!("scheduled_tasks.open_session"),
                                ButtonVariant::Ghost,
                                true,
                                cx,
                                move |_, _, cx| {
                                    cx.emit(ScheduledTasksEvent::OpenSession(session_id))
                                },
                            )
                            .icon(IconName::ArrowUpRight),
                        )
                    })
                    .child(
                        toggle_switch(
                            SharedString::from(format!("scheduled-task-switch-{}", id.0)),
                            enabled,
                            busy,
                            theme,
                            window,
                            cx,
                            move |this, _, cx| this.set_enabled(id, !enabled, cx),
                        )
                        .accessibility_label(format!(
                            "{} · {}",
                            task.title,
                            tr!("scheduled_tasks.enabled")
                        )),
                    )
                    .child(
                        OverflowMenu::new(
                            SharedString::from(format!("scheduled-task-menu-{}", id.0)),
                            menu,
                        )
                        .tooltip(tr!("scheduled_tasks.actions")),
                    ),
            )
            .into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let filtered = !self.tasks.is_empty();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child(icon("icons/rotate-cw.svg", 28.0, theme.text_tertiary))
            .child(
                div()
                    .text_size(ui_px(14.0))
                    .text_color(theme.text_secondary)
                    .child(tr!(if self.loading {
                        "scheduled_tasks.loading"
                    } else if filtered {
                        "scheduled_tasks.no_matches"
                    } else {
                        "scheduled_tasks.empty"
                    })),
            )
            .when(
                !self.loading && !filtered && self.error.is_none(),
                |empty| {
                    empty.child(
                        command_button(
                            "scheduled-empty-new",
                            tr!("scheduled_tasks.new"),
                            ButtonVariant::Primary,
                            !self.busy(),
                            cx,
                            |this, window, cx| this.open_new_task(window, cx),
                        )
                        .icon(IconName::Plus),
                    )
                },
            )
    }
}

impl Render for ScheduledTasksView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let row_height = ui_px(ROW_HEIGHT);
        if row_height != self.row_height {
            self.row_height = row_height;
            let offset = self.list_state.logical_scroll_top();
            self.list_state
                .reset_with_uniform_height(self.visible.len(), row_height);
            self.list_state.scroll_to(offset);
        }
        let theme = Theme::current(cx);
        let weak = cx.entity().downgrade();
        let body = if self.tasks.is_empty() && self.error.is_some() {
            div().into_any_element()
        } else if self.visible.is_empty() {
            self.render_empty(cx).into_any_element()
        } else {
            list(self.list_state.clone(), move |index, window, cx| {
                weak.upgrade()
                    .map(|view| view.update(cx, |view, cx| view.render_task_row(index, window, cx)))
                    .unwrap_or_else(|| div().into_any_element())
            })
            .size_full()
            .into_any_element()
        };
        div()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(theme.surface)
            .relative()
            .flex()
            .flex_col()
            .child(
                div()
                    .size_full()
                    .max_w(px(1040.0))
                    .mx_auto()
                    .px_8()
                    .pt_6()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .child(self.render_toolbar(cx))
                    .when(self.dialog.is_none(), |page| {
                        page.children(
                            self.error
                                .as_ref()
                                .map(|error| self.render_error(error, cx)),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .child(body)
                            .child(scrollbar::vertical(&self.list_state, &self.list_scrollbar)),
                    ),
            )
            .children(self.render_dialog(cx))
    }
}

pub(super) fn command_button(
    id: impl Into<ElementId>,
    label: String,
    variant: ButtonVariant,
    enabled: bool,
    cx: &mut Context<ScheduledTasksView>,
    activate: impl Fn(&mut ScheduledTasksView, &mut Window, &mut Context<ScheduledTasksView>) + 'static,
) -> Button {
    Button::new(id, label)
        .variant(variant)
        .size(ControlSize::Sm)
        .disabled(!enabled)
        .on_click(cx.listener(move |this, _, window, cx| activate(this, window, cx)))
}
