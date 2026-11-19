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
    Preview(SessionCleanupPreview, PathBuf, fintwind_client::DaemonClient),
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
    fn preview_session_cleanup(&mut self, host: Option<Uuid>, cx: &mut Context<Self>) {
        if matches!(
            self.session_cleanup.get(&host).map(|state| &state.phase),
            Some(Phase::Scanning | Phase::Running { .. })
        ) {
            return;
        }
        let client = match host {
            Some(host) => self.ssh_client_for(host),
            None => Some(self.daemon.client()),
        };
        let Some(client) = client else {
            self.session_cleanup.entry(host).or_default().phase = Phase::Failed(tr!("ssh.offline"));
            cx.notify();
            return;
        };
        let binary = host.is_none().then(|| self.native_binary_path()).flatten();
        if host.is_none() && binary.is_none() {
            self.session_cleanup.entry(host).or_default().phase = Phase::Failed(tr!("cleanup.opencode_required"));
            cx.notify();
            return;
        }
        let visible_session_ids: Vec<_> = self
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
        let state = self.session_cleanup.entry(host).or_default();
        let period = state.period;
        state.phase = Phase::Scanning;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let preview_client = client.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let (binary, visible_session_ids) = if let Some(binary) = binary {
                        (binary, visible_session_ids)
                    } else {
                        let (binary, _) = fintwind_client::ssh::opencode_workspace(&client)?;
                        let snapshot = super::runtime::load_remote_task_state(&client)?;
                        let visible = snapshot.sessions.into_iter().filter(|row| !row.archived)
                            .map(|row| row.id).collect();
                        (binary, visible)
                    };
                    match client.request_with_timeout(
                        Uuid::nil(),
                        Uuid::nil(),
                        Command::PreviewSessionCleanup {
                            binary: binary.clone(),
                            period,
                            visible_session_ids,
                        },
                        Duration::from_secs(600),
                    )? {
                        ResponsePayload::SessionCleanupPreview { preview } => Ok((preview, binary)),
                        _ => anyhow::bail!("Invalid session cleanup preview"),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.session_cleanup.entry(host).or_default().phase = match result {
                    Ok((preview, binary)) => Phase::Preview(preview, binary, preview_client),
                    Err(error) => {
                        let message = error.to_string();
                        if host.is_some() {
                            this.show_toast(message.clone());
                        }
                        Phase::Failed(message)
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_session_cleanup(&mut self, host: Option<Uuid>, cx: &mut Context<Self>) {
        let Some(Phase::Preview(preview, binary, client)) = self.session_cleanup.get(&host).map(|state| &state.phase) else {
            return;
        };
        if preview.session_ids.is_empty() {
            return;
        }
        let preview = preview.clone();
        let binary = binary.clone();
        let client = client.clone();
        if client.is_disconnected() {
            self.session_cleanup.entry(host).or_default().phase = Phase::Failed(tr!("ssh.offline"));
            cx.notify();
            return;
        }
        let total = preview.session_ids.len();
        self.session_cleanup.entry(host).or_default().phase = Phase::Running {
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
                        this.session_cleanup.entry(host).or_default().phase = Phase::Running {
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
                    Ok(snapshot) => match host {
                        Some(host) => this.apply_ssh_cleanup_snapshot(host, snapshot, cx),
                        None => this.apply_remote_task_state(snapshot, cx),
                    },
                    Err(error) => errors.push(error.to_string()),
                }
                if host.is_some() {
                    let message = tr!("cleanup.finished", deleted = deleted, skipped = skipped, failed = errors.len());
                    if errors.is_empty() {
                        this.show_success_toast(message);
                    } else {
                        this.show_toast(message);
                    }
                }
                this.session_cleanup.entry(host).or_default().phase = Phase::Finished {
                    deleted,
                    skipped,
                    errors,
                };
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_session_cleanup(&self, host: Option<Uuid>, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let state = self.session_cleanup.get(&host);
        let phase = state.map_or(&Phase::Idle, |state| &state.phase);
        let busy = matches!(phase, Phase::Scanning | Phase::Running { .. });
        let offline = host.is_some_and(|host| self.ssh_client_for(host).is_none());
        let period = state.map_or(RetentionPeriod::default(), |state| state.period);
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
                                let state = this.session_cleanup.entry(host).or_default();
                                if matches!(state.phase, Phase::Scanning | Phase::Running { .. }) {
                                    return;
                                }
                                state.period = value;
                                state.phase = Phase::Idle;
                                cx.notify();
                            });
                        })
                        .selected(value == period)
                    })
                    .collect()
            },
        );
        let status = match phase {
            Phase::Idle => None,
            Phase::Scanning => Some(tr!("cleanup.scanning")),
            Phase::Preview(preview, ..) if preview.session_ids.is_empty() => Some(tr!("cleanup.empty")),
            Phase::Preview(preview, ..) => {
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
        let error = match phase {
            Phase::Finished { errors, .. } => errors.first().cloned(),
            _ => None,
        };
        div().w_full().px(px(20.0)).py(px(14.0)).flex().flex_col()
            .child(div().flex().items_center().gap(px(14.0))
                .child(super::settings::settings_icon_chip("icons/trash.svg", theme))
                .child(super::settings::settings_row_text(tr!("cleanup.title"), Some(tr!("cleanup.scope")), theme))
                .child(selector)
                .child(Button::new("cleanup-preview", tr!("cleanup.preview"))
                    .variant(ButtonVariant::Secondary).size(ControlSize::Sm).disabled(busy || offline)
                    .on_click(cx.listener(move |this, _, _, cx| this.preview_session_cleanup(host, cx)))))
            .children(status.map(|status| div().mt(px(12.0)).text_size(ui_px(12.5)).text_color(theme.text_secondary).child(status)))
            .children(error.map(|error| div().mt(px(6.0)).text_size(ui_px(12.0)).text_color(theme.danger_text).child(error)))
            .when(matches!(phase, Phase::Preview(preview, ..) if !preview.session_ids.is_empty()), |panel| {
                panel.child(div().mt(px(12.0)).flex().gap(px(8.0))
                    .child(Button::new("cleanup-confirm", tr!("cleanup.delete"))
                        .variant(ButtonVariant::Danger).size(ControlSize::Sm)
                        .disabled(offline)
                        .on_click(cx.listener(move |this, _, _, cx| this.confirm_session_cleanup(host, cx))))
                    .child(Button::new("cleanup-cancel", tr!("cleanup.cancel"))
                        .variant(ButtonVariant::Ghost).size(ControlSize::Sm)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.session_cleanup.entry(host).or_default().phase = Phase::Idle;
                            cx.notify();
                        }))))
            })
    }
}
