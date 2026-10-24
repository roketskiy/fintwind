use super::*;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    theme::ControlSize,
};
use fintwind_client::{Command, ResponsePayload};
use fintwind_protocol::session_cleanup::{
    RetentionPeriod, SessionCleanupOutcome, SessionCleanupPreview,
};

#[derive(Default)]
pub(super) struct CleanupState {
    period: RetentionPeriod,
    phase: Phase,
}

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Scanning,
    Preview(SessionCleanupPreview),
    Running {
        completed: usize,
        total: usize,
    },
    Finished {
        deleted: usize,
        skipped: usize,
        errors: Vec<String>,
    },
    Failed(String),
}

impl Fintwind {
    fn preview_session_cleanup(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.session_cleanup.phase,
            Phase::Scanning | Phase::Running { .. }
        ) {
            return;
        }
        let Some(binary) = self.native_binary_path() else {
            self.session_cleanup.phase = Phase::Failed(tr!("cleanup.opencode_required"));
            cx.notify();
            return;
        };
        let visible_session_ids = self
            .state
            .sessions
            .iter()
            .filter(|row| {
                !row.archived
                    && !self.runtimes.contains_key(&row.id)
                    && !self.submission_preparations.contains(&row.id)
                    && !self.response_fork_preparations.contains_key(&row.id)
                    && !self.undo_redo_preparations.contains(&row.id)
            })
            .map(|row| row.id)
            .collect();
        let period = self.session_cleanup.period;
        let client = self.daemon.client();
        self.session_cleanup.phase = Phase::Scanning;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match client.request_with_timeout(
                        Uuid::nil(),
                        Uuid::nil(),
                        Command::PreviewSessionCleanup {
                            binary,
                            period,
                            visible_session_ids,
                        },
                        Duration::from_secs(600),
                    )? {
                        ResponsePayload::SessionCleanupPreview { preview } => Ok(preview),
                        _ => anyhow::bail!("Invalid session cleanup preview"),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.session_cleanup.phase = match result {
                    Ok(preview) => Phase::Preview(preview),
                    Err(error) => Phase::Failed(error.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_session_cleanup(&mut self, cx: &mut Context<Self>) {
        let Phase::Preview(preview) = &self.session_cleanup.phase else {
            return;
        };
        if preview.session_ids.is_empty() {
            return;
        }
        let preview = preview.clone();
        let Some(binary) = self.native_binary_path() else {
            return;
        };
        let client = self.daemon.client();
        let total = preview.session_ids.len();
        self.session_cleanup.phase = Phase::Running {
            completed: 0,
            total,
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut deleted = 0;
            let mut skipped = 0;
            let mut errors = Vec::new();
            for (index, session_id) in preview.session_ids.into_iter().enumerate() {
                let client = client.clone();
                let binary = binary.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        client.request_with_timeout(
                            session_id,
                            Uuid::nil(),
                            Command::CleanupOldSession {
                                binary,
                                before: preview.before,
                            },
                            Duration::from_secs(600),
                        )
                    })
                    .await;
                match result {
                    Ok(ResponsePayload::SessionCleanupFinished {
                        outcome: SessionCleanupOutcome::Deleted { session_ids },
                    }) => deleted += session_ids.len(),
                    Ok(ResponsePayload::SessionCleanupFinished {
                        outcome: SessionCleanupOutcome::Skipped,
                    }) => skipped += 1,
                    Ok(_) => errors.push("Invalid session cleanup response".to_owned()),
                    Err(error) => errors.push(format!("{session_id}: {error}")),
                }
                if this
                    .update(cx, |this, cx| {
                        this.session_cleanup.phase = Phase::Running {
                            completed: index + 1,
                            total,
                        };
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            let snapshot = cx
                .background_executor()
                .spawn(async move {
                    client.request(Uuid::nil(), Uuid::nil(), Command::SweepBlobs)?;
                    super::runtime::load_remote_task_state(&client)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match snapshot {
                    Ok(snapshot) => this.apply_remote_task_state(snapshot, cx),
                    Err(error) => errors.push(error.to_string()),
                }
                this.session_cleanup.phase = Phase::Finished {
                    deleted,
                    skipped,
                    errors,
                };
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_session_cleanup(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let busy = matches!(
            self.session_cleanup.phase,
            Phase::Scanning | Phase::Running { .. }
        );
        let period = self.session_cleanup.period;
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("cleanup-retention", cx);
        let selector = dropdown_menu(
            MenuChip::new("cleanup-retention")
                .label(tr!("cleanup.days", days = period.days()))
                .outlined()
                .selected(handle.is_open())
                .disabled(busy),
            "cleanup-retention-menu",
            &handle,
            MenuAlign::BelowRight,
            move |_| {
                RetentionPeriod::ALL
                    .into_iter()
                    .map(|value| {
                        let weak = weak.clone();
                        MenuItem::new(tr!("cleanup.days", days = value.days()), move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                if matches!(
                                    this.session_cleanup.phase,
                                    Phase::Scanning | Phase::Running { .. }
                                ) {
                                    return;
                                }
                                this.session_cleanup.period = value;
                                this.session_cleanup.phase = Phase::Idle;
                                cx.notify();
                            });
                        })
                        .selected(value == period)
                    })
                    .collect()
            },
        );
        let status = match &self.session_cleanup.phase {
            Phase::Idle => None,
            Phase::Scanning => Some(tr!("cleanup.scanning")),
            Phase::Preview(preview) if preview.session_ids.is_empty() => Some(tr!("cleanup.empty")),
            Phase::Preview(preview) => {
                Some(tr!("cleanup.confirm", count = preview.session_ids.len()))
            }
            Phase::Running { completed, total } => Some(tr!(
                "cleanup.progress",
                completed = completed,
                total = total
            )),
            Phase::Finished {
                deleted,
                skipped,
                errors,
            } => Some(tr!(
                "cleanup.finished",
                deleted = deleted,
                skipped = skipped,
                failed = errors.len()
            )),
            Phase::Failed(error) => Some(error.clone()),
        };
        let error = match &self.session_cleanup.phase {
            Phase::Finished { errors, .. } => errors.first().cloned(),
            _ => None,
        };
        div().w_full().p(px(20.0))
            .child(div().flex().items_center().gap(px(12.0))
                .child(div().flex_1().text_size(ui_px(13.5)).font_weight(FontWeight::MEDIUM).child(tr!("cleanup.title")))
                .child(selector)
                .child(Button::new("cleanup-preview", tr!("cleanup.preview"))
                    .variant(ButtonVariant::Secondary).size(ControlSize::Sm).disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.preview_session_cleanup(cx)))))
            .child(div().mt(px(6.0)).text_size(ui_px(12.5)).text_color(theme.text_secondary).child(tr!("cleanup.scope")))
            .children(status.map(|status| div().mt(px(12.0)).text_size(ui_px(12.5)).text_color(theme.text_secondary).child(status)))
            .children(error.map(|error| div().mt(px(6.0)).text_size(ui_px(12.0)).text_color(theme.danger_text).child(error)))
            .when(matches!(&self.session_cleanup.phase, Phase::Preview(preview) if !preview.session_ids.is_empty()), |panel| {
                panel.child(div().mt(px(12.0)).flex().gap(px(8.0))
                    .child(Button::new("cleanup-confirm", tr!("cleanup.delete"))
                        .variant(ButtonVariant::Danger).size(ControlSize::Sm)
                        .on_click(cx.listener(|this, _, _, cx| this.confirm_session_cleanup(cx))))
                    .child(Button::new("cleanup-cancel", tr!("cleanup.cancel"))
                        .variant(ButtonVariant::Ghost).size(ControlSize::Sm)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.session_cleanup.phase = Phase::Idle;
                            cx.notify();
                        }))))
            })
    }
}
