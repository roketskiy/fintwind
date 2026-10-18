use super::*;
use crate::theme::ui_px;
use crate::ui::ActivationExt;
use fintwind_client::goal::{GoalAction, GoalStatus};

impl Fintwind {
    pub(super) fn stage_goal(&mut self, edit: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.goal_enabled {
            self.show_toast(tr!("goal.enable_first"));
            cx.notify();
            return;
        }
        if !self.composer.read(cx).content().trim().is_empty()
            || !self.composer_attachments.is_empty()
            || !self.composer_references.is_empty()
        {
            self.show_toast(tr!("goal.draft_present"));
            window.focus(&self.composer.read(cx).focus(), cx);
            cx.notify();
            return;
        }
        let objective = edit
            .then(|| {
                self.selected_session()
                    .and_then(|session| session.goal.as_ref())
                    .map(|goal| goal.objective.clone())
            })
            .flatten()
            .unwrap_or_default();
        self.composer.update(cx, |input, cx| {
            input.set_content(format!("/goal {objective}"), cx)
        });
        window.focus(&self.composer.read(cx).focus(), cx);
        self.schedule_composer_draft_save(cx);
        cx.notify();
    }

    pub(super) fn control_goal(
        &mut self,
        session_id: Uuid,
        action: GoalAction,
        cx: &mut Context<Self>,
    ) {
        if !self.state.goal_enabled
            && matches!(action, GoalAction::Start { .. } | GoalAction::Resume)
        {
            self.show_toast(tr!("goal.enable_first"));
            cx.notify();
            return;
        }
        if matches!(action, GoalAction::Start { .. } | GoalAction::Resume)
            && self
                .state
                .sessions
                .iter()
                .any(|session| session.id == session_id && session.is_busy())
        {
            self.show_toast(tr!("goal.wait_for_turn"));
            cx.notify();
            return;
        }
        if let Some(runtime) = self.runtimes.get(&session_id) {
            let driver = runtime.driver.clone();
            if let Some(session) = self.state.session_mut(session_id) {
                match &action {
                    GoalAction::Start { objective } => {
                        session.begin_turn(objective.clone());
                        session.mark_active_turn_goal();
                        session.status = SessionStatus::Connecting;
                        session.updated_at = unix_time();
                    }
                    GoalAction::Resume => {
                        session.begin_provider_turn();
                        session.mark_active_turn_goal();
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
        if !self.state.goal_enabled {
            self.restore_composer_submission(submission.clone(), cx);
            self.show_toast(tr!("goal.enable_first"));
            return true;
        }
        let Some(session_id) = self.state.selected_session else {
            self.restore_composer_submission(submission.clone(), cx);
            return true;
        };
        if !submission.attachments.is_empty() {
            self.restore_composer_submission(submission.clone(), cx);
            self.show_toast(tr!("goal.no_attachments"));
            return true;
        }
        let argument = prompt[command_end..].trim();
        let control_only = matches!(
            argument.to_ascii_lowercase().as_str(),
            "" | "status" | "pause" | "complete" | "clear"
        );
        if !control_only
            && (self.pending_goal_actions.contains_key(&session_id)
                || self
                    .selected_session()
                    .is_some_and(|session| session.is_busy()))
        {
            self.restore_composer_submission(submission.clone(), cx);
            self.show_toast(tr!("goal.wait_for_turn"));
            return true;
        }
        let action = match argument.to_ascii_lowercase().as_str() {
            "" | "status" => GoalAction::Get,
            "pause" => GoalAction::Pause,
            "resume" => GoalAction::Resume,
            "complete" => GoalAction::Complete,
            "clear" => GoalAction::Clear,
            _ => {
                if let Some(session) = self.state.session_mut(session_id) {
                    session.set_title_from_prompt(argument);
                }
                GoalAction::Start {
                    objective: argument.to_owned(),
                }
            }
        };
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
        if self.state.goal_enabled {
            commands.push(SlashCommand {
                name: "goal".to_owned(),
                description: tr!("goal.set"),
                scope: crate::composer_complete::CommandScope::Builtin,
                argument_hint: Some(tr!("goal.title")),
                template: None,
            });
        }
        commands.sort_by(|a, b| {
            (a.scope.display_rank(), &a.name).cmp(&(b.scope.display_rank(), &b.name))
        });
        commands
    }

    pub(super) fn render_goal(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.state.goal_enabled {
            return None;
        }
        let session = self.selected_session()?;
        let goal = session
            .goal
            .as_ref()
            .filter(|goal| goal.status != GoalStatus::Done)?;
        let session_id = session.id;
        let expanded = self.expanded_goal == Some(session_id);
        let active = goal.status == GoalStatus::Active;
        let theme = Theme::current(cx);
        let summary_focus = self.menu_handle("goal-summary", cx);
        let action_focus = self.menu_handle("goal-primary", cx);
        let details_focus = self.menu_handle("goal-details", cx);
        let summary: String = goal
            .objective
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(160)
            .collect();
        let status = match goal.status {
            GoalStatus::Active => tr!("goal.active"),
            GoalStatus::Paused => tr!("goal.paused"),
            GoalStatus::Done => tr!("goal.done"),
            GoalStatus::Blocked => tr!("goal.blocked"),
        };
        let status_color = theme.text_secondary;
        let duration = goal.elapsed_seconds_at(unix_time()).map(|elapsed| {
            tr!(
                "goal.card_duration",
                duration = Self::format_goal_duration(elapsed)
            )
        });
        let action = if active {
            GoalAction::Pause
        } else {
            GoalAction::Resume
        };
        let primary = div()
            .id("goal-primary")
            .track_focus(action_focus.trigger_focus_handle())
            .tab_index(0)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .h(px(28.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .rounded(px(6.0))
            .cursor_default()
            .child(icon(
                if active {
                    "icons/pause.svg"
                } else {
                    "icons/play.svg"
                },
                12.0,
                theme.text_secondary,
            ))
            .child(if active {
                tr!("goal.pause")
            } else {
                tr!("goal.resume")
            })
            .on_activation(cx, move |this, _, cx| {
                this.control_goal(session_id, action.clone(), cx)
            });
        let weak = cx.entity().downgrade();
        let menu_handle = self.menu_handle("goal-actions", cx);
        let more = dropdown_menu(
            MenuChip::new("goal-actions")
                .icon("icons/ellipsis.svg", theme.text_secondary)
                .icon_only(true)
                .caret(false)
                .tooltip(tr!("goal.actions")),
            "goal-actions-menu",
            &menu_handle,
            MenuAlign::AboveRight,
            move |_| {
                let edit = weak.clone();
                let complete = weak.clone();
                let clear = weak.clone();
                vec![
                    MenuItem::new(tr!("goal.edit"), move |window, cx| {
                        let _ = edit.update(cx, |this, cx| this.stage_goal(true, window, cx));
                    })
                    .icon("icons/pencil.svg"),
                    MenuItem::new(tr!("goal.complete"), move |_, cx| {
                        let _ = complete.update(cx, |this, cx| {
                            this.control_goal(session_id, GoalAction::Complete, cx)
                        });
                    })
                    .icon("icons/check.svg"),
                    MenuItem::Separator,
                    MenuItem::new(tr!("goal.clear"), move |_, cx| {
                        let _ = clear.update(cx, |this, cx| {
                            this.control_goal(session_id, GoalAction::Clear, cx)
                        });
                    })
                    .icon("icons/x.svg"),
                ]
            },
        );
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
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .id("goal-summary")
                                .track_focus(summary_focus.trigger_focus_handle())
                                .tab_index(0)
                                .focus_visible(|style| style.border_1().border_color(theme.accent))
                                .flex_1()
                                .min_w_0()
                                .h(px(28.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .rounded(px(6.0))
                                .cursor_default()
                                .child(icon("icons/target.svg", 14.0, theme.text_secondary))
                                .child(tr!("goal.title"))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .text_color(theme.text)
                                        .child(summary),
                                )
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
                        .child(
                            div()
                                .flex_none()
                                .text_color(status_color)
                                .child(status),
                        )
                        .when_some(duration, |row, duration| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_color(theme.text_tertiary)
                                    .child(duration),
                            )
                        })
                        .when(goal.status != GoalStatus::Done, |row| row.child(primary))
                        .child(more),
                )
                .when(expanded, |banner| {
                    banner.child(
                        div()
                            .id("goal-details")
                            .track_focus(details_focus.trigger_focus_handle())
                            .tab_index(0)
                            .focus_visible(|style| style.border_1().border_color(theme.accent))
                            .max_h(px(160.0))
                            .overflow_y_scroll()
                            .track_scroll(&self.goal_details_scroll)
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                let mut offset = this.goal_details_scroll.offset();
                                let max = this.goal_details_scroll.max_offset();
                                match event.keystroke.key.as_str() {
                                    "up" => offset.y += px(24.0),
                                    "down" => offset.y -= px(24.0),
                                    "pageup" => offset.y += px(120.0),
                                    "pagedown" => offset.y -= px(120.0),
                                    "home" => offset.y = px(0.0),
                                    "end" => offset.y = -max.y,
                                    "escape" => {
                                        this.expanded_goal = None;
                                        cx.notify();
                                        cx.stop_propagation();
                                        return;
                                    }
                                    _ => return,
                                }
                                offset.y = offset.y.clamp(-max.y, px(0.0));
                                this.goal_details_scroll.set_offset(offset);
                                cx.notify();
                                cx.stop_propagation();
                            }))
                            .pt(px(5.0))
                            .pb(px(3.0))
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .child(div().text_color(theme.text).child(goal.objective.clone()))
                            .child(tr!("goal.card_run", count = goal.turns))
                            .when(!goal.reason.is_empty(), |details| {
                                details.child(tr!(
                                    "goal.last_result",
                                    reason = goal.reason.clone()
                                ))
                            }),
                    )
                }),
        )
    }
}
