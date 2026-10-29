use super::*;
use crate::theme::ui_px;
use crate::ui::ActivationExt;
use fintwind_client::goal::{GoalAction, GoalStatus};

/// Match the single layer of quotes stripped by OpenCode's Goal command.
fn goal_argument(text: &str) -> &str {
    let text = text.trim();
    for (open, close) in [("\"", "\""), ("'", "'"), ("“", "”"), ("‘", "’")] {
        if let Some(inner) = text
            .strip_prefix(open)
            .and_then(|text| text.strip_suffix(close))
        {
            return inner.trim();
        }
    }
    text
}

impl Fintwind {
    pub(super) fn control_goal(
        &mut self,
        session_id: Uuid,
        action: GoalAction,
        cx: &mut Context<Self>,
    ) {
        if let Some(runtime) = self.runtimes.get(&session_id) {
            let driver = runtime.driver.clone();
            if let Some(session) = self.state.session_mut(session_id) {
                match &action {
                    GoalAction::Start {
                        objective,
                        attachments,
                    } => {
                        if session.is_busy() {
                            session.push_user_message_with_presentation(
                                goal_argument(objective).to_owned(),
                                None,
                                attachments.clone(),
                            );
                        } else {
                            session.begin_turn_with_presentation(
                                goal_argument(objective).to_owned(),
                                None,
                                attachments.clone(),
                            );
                            session.mark_active_turn_goal();
                            session.status = SessionStatus::Connecting;
                        }
                        session.updated_at = unix_time();
                    }
                    GoalAction::Resume if !session.is_busy() => {
                        session.begin_provider_turn();
                    }
                    _ => {}
                }
            }
            driver.goal(action);
            if self.state.selected_session == Some(session_id) {
                self.sync_transcript_rows();
            }
        } else {
            self.pending_goal_actions.insert(session_id, action);
            self.start_runtime_attachment(session_id, cx);
        }
        cx.notify();
    }

    pub(super) fn try_submit_goal(
        &mut self,
        submission: &ComposerSubmission,
        cx: &mut Context<Self>,
    ) -> bool {
        let prompt = submission.prompt.trim();
        let command_end = prompt.find(char::is_whitespace).unwrap_or(prompt.len());
        if !prompt[..command_end].eq_ignore_ascii_case("/goal") {
            return false;
        }
        let Some(session_id) = self.state.selected_session else {
            self.restore_composer_submission(submission.clone(), cx);
            return true;
        };
        let raw_argument = prompt[command_end..].trim();
        let argument = goal_argument(raw_argument);
        if self.pending_goal_actions.contains_key(&session_id)
            || self.submission_preparations.contains(&session_id)
        {
            self.restore_composer_submission(submission.clone(), cx);
            self.show_toast(tr!("goal.reconnecting"));
            return true;
        }
        let action = match argument.to_ascii_lowercase().as_str() {
            "" => GoalAction::Get,
            "pause" => GoalAction::Pause,
            "resume" => GoalAction::Resume,
            "clear" => GoalAction::Clear,
            _ => {
                if let Some(session) = self.state.session_mut(session_id) {
                    session.set_title_from_prompt(argument);
                }
                GoalAction::Start {
                    objective: raw_argument.to_owned(),
                    attachments: submission.attachments.clone(),
                }
            }
        };
        if matches!(action, GoalAction::Get) {
            self.goal_status_requests.insert(session_id);
        }
        self.control_goal(session_id, action, cx);
        self.save();
        true
    }

    pub(super) fn composer_goal_commands(
        &self,
        discovered: &[SlashCommand],
        reported: &[crate::model::ReportedCommand],
    ) -> Vec<SlashCommand> {
        let mut commands = super::btw::commands(discovered, reported);
        commands.retain(|command| command.name != "goal");
        commands.push(SlashCommand {
            name: "goal".to_owned(),
            description: tr!("goal.set"),
            scope: crate::composer_complete::CommandScope::Builtin,
            argument_hint: Some(tr!("goal.title")),
            template: None,
        });
        commands.sort_by(|a, b| {
            (a.scope.display_rank(), &a.name).cmp(&(b.scope.display_rank(), &b.name))
        });
        commands
    }

    pub(super) fn render_goal(&self, cx: &mut Context<Self>) -> Option<Div> {
        let session = self.selected_session()?;
        let goal = session.goal.as_ref()?;
        let session_id = session.id;
        let expanded = self.expanded_goal == Some(session_id);
        let theme = Theme::current(cx);
        let summary_focus = self.menu_handle("goal-summary", cx);
        let action_focus = self.menu_handle("goal-primary", cx);
        let clear_focus = self.menu_handle("goal-clear", cx);
        let details_focus = self.menu_handle("goal-details", cx);
        let (status, status_icon, status_color, primary) = match goal.status {
            GoalStatus::Active => (
                tr!("goal.title"),
                "icons/target.svg",
                theme.text_secondary,
                Some((GoalAction::Pause, "icons/pause.svg", tr!("goal.pause"))),
            ),
            GoalStatus::Paused | GoalStatus::Blocked => (
                tr!("goal.paused"),
                "icons/alert.svg",
                theme.warning_text,
                Some((
                    GoalAction::Resume,
                    "icons/rotate-cw.svg",
                    tr!("goal.resume"),
                )),
            ),
            GoalStatus::Done => (
                tr!("goal.done"),
                "icons/check.svg",
                theme.success_text,
                None,
            ),
        };
        Some(
            div()
                .px(px(10.0))
                .pb(px(8.0))
                .mb(px(6.0))
                .border_b_1()
                .border_color(theme.border)
                .text_size(ui_px(12.0))
                .text_color(theme.text_secondary)
                .child(
                    div()
                        .id("goal-summary")
                        .track_focus(summary_focus.trigger_focus_handle())
                        .tab_index(0)
                        .focus_visible(|style| style.border_1().border_color(theme.accent))
                        .h(px(28.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .rounded(px(6.0))
                        .cursor_default()
                        .text_color(status_color)
                        .child(icon(status_icon, 14.0, status_color))
                        .child(status)
                        .child(icon(
                            if expanded {
                                "icons/chevron-up.svg"
                            } else {
                                "icons/chevron-down.svg"
                            },
                            11.0,
                            theme.text_tertiary,
                        ))
                        .on_activation(cx, move |this, _, cx| {
                            this.expanded_goal = if this.expanded_goal == Some(session_id) {
                                None
                            } else {
                                Some(session_id)
                            };
                            cx.notify();
                        }),
                )
                .when(expanded, |banner| {
                    banner.child(
                        div()
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape" {
                                    this.expanded_goal = None;
                                    cx.notify();
                                    cx.stop_propagation();
                                }
                            }))
                            .pt(px(5.0))
                            .pb(px(3.0))
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .child(
                                div()
                                    .id("goal-details")
                                    .track_focus(details_focus.trigger_focus_handle())
                                    .tab_index(0)
                                    .focus_visible(|style| {
                                        style.border_1().border_color(theme.accent)
                                    })
                                    .max_h(px(160.0))
                                    .overflow_y_scroll()
                                    .track_scroll(&self.goal_details_scroll)
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, _, cx| {
                                            let mut offset = this.goal_details_scroll.offset();
                                            let max = this.goal_details_scroll.max_offset();
                                            match event.keystroke.key.as_str() {
                                                "up" => offset.y += px(24.0),
                                                "down" => offset.y -= px(24.0),
                                                "pageup" => offset.y += px(120.0),
                                                "pagedown" => offset.y -= px(120.0),
                                                "home" => offset.y = px(0.0),
                                                "end" => offset.y = -max.y,
                                                _ => return,
                                            }
                                            offset.y = offset.y.clamp(-max.y, px(0.0));
                                            this.goal_details_scroll.set_offset(offset);
                                            cx.notify();
                                            cx.stop_propagation();
                                        },
                                    ))
                                    .text_color(theme.text)
                                    .child(goal.objective.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(4.0))
                                    .when_some(primary, |actions, (action, icon_path, label)| {
                                        actions.child(
                                            div()
                                                .id("goal-primary")
                                                .track_focus(action_focus.trigger_focus_handle())
                                                .tab_index(0)
                                                .focus_visible(|style| {
                                                    style.border_1().border_color(theme.accent)
                                                })
                                                .h(px(28.0))
                                                .px(px(8.0))
                                                .flex()
                                                .items_center()
                                                .gap(px(5.0))
                                                .rounded(px(6.0))
                                                .cursor_default()
                                                .child(icon(icon_path, 12.0, theme.text_secondary))
                                                .child(label)
                                                .on_activation(cx, move |this, _, cx| {
                                                    this.expanded_goal = None;
                                                    this.control_goal(
                                                        session_id,
                                                        action.clone(),
                                                        cx,
                                                    );
                                                }),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id("goal-clear")
                                            .track_focus(clear_focus.trigger_focus_handle())
                                            .tab_index(0)
                                            .focus_visible(|style| {
                                                style.border_1().border_color(theme.accent)
                                            })
                                            .h(px(28.0))
                                            .px(px(8.0))
                                            .flex()
                                            .items_center()
                                            .gap(px(5.0))
                                            .rounded(px(6.0))
                                            .cursor_default()
                                            .child(icon(
                                                "icons/trash.svg",
                                                12.0,
                                                theme.text_secondary,
                                            ))
                                            .child(tr!("goal.clear"))
                                            .on_activation(cx, move |this, _, cx| {
                                                this.expanded_goal = None;
                                                this.control_goal(
                                                    session_id,
                                                    GoalAction::Clear,
                                                    cx,
                                                );
                                            }),
                                    ),
                            ),
                    )
                }),
        )
    }
}
