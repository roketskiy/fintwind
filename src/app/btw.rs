//! Native, window-local side questions. No turns, inbox items or driver events.

use gpui::{KeyBinding, actions};

use crate::md::virtualized::ReasoningView;
use crate::ui::ActivationExt;

use super::*;

actions!(fintwind_btw, [DismissBtw]);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", DismissBtw, Some("BtwPanel"))]);
}

pub(super) fn question(prompt: &str) -> Option<&str> {
    let prompt = prompt.trim();
    let (command, arguments) = prompt
        .split_once(char::is_whitespace)
        .unwrap_or((prompt, ""));
    (command == "/btw").then(|| arguments.trim())
}

pub(super) fn commands(
    discovered: &[SlashCommand],
    reported: &[crate::model::ReportedCommand],
) -> Vec<SlashCommand> {
    let mut commands = crate::composer_complete::merge_reported_commands(discovered, reported);
    commands.retain(|command| command.name != "btw");
    commands.push(SlashCommand {
        name: "btw".to_owned(),
        description: tr!("btw.command_description"),
        scope: crate::composer_complete::CommandScope::Builtin,
        argument_hint: Some(tr!("btw.argument_hint")),
        template: None,
    });
    commands
        .sort_by(|a, b| (a.scope.display_rank(), &a.name).cmp(&(b.scope.display_rank(), &b.name)));
    commands
}

struct PendingGeneration {
    id: Uuid,
    session_id: Uuid,
    client: fintwind_client::DaemonClient,
    cancel_on_drop: bool,
}

impl Drop for PendingGeneration {
    fn drop(&mut self) {
        if !self.cancel_on_drop {
            return;
        }
        // The outgoing queue is non-blocking. This also retires work when the
        // window/session/tab is dropped, without interrupting the main runner.
        let _ = self.client.notify(
            self.session_id,
            Uuid::nil(),
            fintwind_client::Command::CancelSessionGeneration {
                generation_id: self.id,
            },
        );
    }
}

pub(super) struct BtwState {
    question: String,
    input: Entity<ComposerInput>,
    answer: String,
    error: Option<String>,
    request: Option<PendingGeneration>,
    markdown: Option<Entity<ReasoningView>>,
    selection: TranscriptSelection,
    focus: FocusHandle,
    copy_focus: FocusHandle,
    action_focus: FocusHandle,
}

impl BtwState {
    pub(super) fn selected_text(&self) -> Option<String> {
        self.selection.selection.borrow().selected_text()
    }

    pub(super) fn clear_selection(&self) {
        self.selection.selection.borrow_mut().clear();
        self.selection.registry.borrow_mut().clear();
    }
}

impl Fintwind {
    /// All submission paths call this before busy/fork/undo queueing. Invalid
    /// side questions restore text and attachments rather than silently losing them.
    pub(super) fn try_submit_btw(
        &mut self,
        submission: &ComposerSubmission,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(result) = self.submit_btw_question(
            &submission.prompt,
            submission.attachments.len(),
            self.composer.clone(),
            cx,
        ) else {
            return false;
        };
        if let Err(error) = result {
            self.restore_composer_submission(submission.clone(), cx);
            self.show_toast(error);
        }
        true
    }

    /// Shared by the main composer and history editors; restoration belongs to
    /// each caller so rejecting a side question cannot overwrite another field.
    pub(super) fn submit_btw_question(
        &mut self,
        prompt: &str,
        attachments: usize,
        input: Entity<ComposerInput>,
        cx: &mut Context<Self>,
    ) -> Option<Result<(), String>> {
        let question = question(prompt)?;
        Some(if question.is_empty() {
            Err(tr!("btw.question_required"))
        } else if attachments > 0 {
            Err(tr!("btw.attachments_unsupported"))
        } else {
            self.ask_btw(question.to_owned(), input, cx)
        })
    }

    fn ask_btw(
        &mut self,
        question: String,
        input: Entity<ComposerInput>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .selected_session()
            .ok_or_else(|| tr!("btw.session_required"))?;
        let session_id = session.id;
        if self.btw_options_pending.contains(&session_id) {
            return Err(tr!("btw.model_not_synced"));
        }
        let options = self.session_options(session);
        let model =
            options.model.map(
                |model| fintwind_client::provider_session::SessionGenerationModel {
                    model,
                    variant: options.reasoning_effort,
                },
            );
        let native_id = session
            .native_session_id
            .clone()
            .or_else(|| {
                session
                    .provider_cursor
                    .as_ref()
                    .map(|cursor| cursor.native_id().to_owned())
            })
            .ok_or_else(|| tr!("btw.session_required"))?;
        // Probe paths belong to the daemon host, so do not probe them on this
        // desktop (especially when attached to a remote daemon).
        let binary = self
            .probes
            .first()
            .and_then(|probe| probe.path.clone())
            .ok_or_else(|| tr!("btw.unavailable"))?;
        let directory = self
            .workspace_path_for_session(session)
            .map(Path::to_path_buf)
            .ok_or_else(|| tr!("btw.unavailable"))?;
        let id = Uuid::new_v4();
        let client = self.daemon.client();
        self.btw_states.insert(
            session_id,
            BtwState {
                question: question.clone(),
                input,
                answer: String::new(),
                error: None,
                request: Some(PendingGeneration {
                    id,
                    session_id,
                    client: client.clone(),
                    cancel_on_drop: true,
                }),
                markdown: None,
                selection: TranscriptSelection::default(),
                focus: cx.focus_handle(),
                copy_focus: cx.focus_handle(),
                action_focus: cx.focus_handle(),
            },
        );
        self.open_right_panel_surface(RightPanelSurface::Btw, cx);
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move {
                    match client.request_with_timeout(
                        session_id,
                        Uuid::nil(),
                        fintwind_client::Command::GenerateSessionText {
                            generation_id: id,
                            binary,
                            directory,
                            session_id: native_id,
                            question,
                            model,
                        },
                        Duration::from_secs(135),
                    )? {
                        fintwind_client::ResponsePayload::SessionTextGenerated { text } => {
                            Ok(Some(text))
                        }
                        fintwind_client::ResponsePayload::SessionGenerationModelNotSynced => {
                            Ok(None)
                        }
                        _ => anyhow::bail!(tr!("btw.invalid_response")),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_btw(session_id, id, answer, cx));
        })
        .detach();
        Ok(())
    }

    fn finish_btw(
        &mut self,
        session_id: Uuid,
        id: Uuid,
        result: anyhow::Result<Option<String>>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.btw_states.get_mut(&session_id).filter(|state| {
            state
                .request
                .as_ref()
                .is_some_and(|request| request.id == id)
        }) else {
            return;
        };
        if let Some(request) = state.request.as_mut() {
            request.cancel_on_drop = false;
        }
        state.request = None;
        match result {
            Ok(Some(answer)) if !answer.trim().is_empty() => {
                let selection = state.selection.clone();
                let weak = cx.entity().downgrade();
                let link = Rc::new(move |url: &str, _: &mut Window, cx: &mut App| {
                    let handled = weak
                        .update(cx, |this, cx| {
                            this.state.selected_session == Some(session_id)
                                && this.open_transcript_link(url, cx)
                        })
                        .unwrap_or(false);
                    if !handled {
                        cx.open_url(url);
                    }
                });
                state.markdown = Some(cx.new(|cx| {
                    let mut view = ReasoningView::new(
                        format!("btw-{session_id}-{id}"),
                        selection,
                        link,
                        Some(px(0.0)),
                        cx,
                    )
                    .with_height(gpui::relative(1.0));
                    view.set_source(&answer, (1, 1), false, cx);
                    view
                }));
                state.answer = answer;
            }
            Ok(None) => {
                state.error = Some(tr!("btw.model_not_synced"));
                // A read-only guard may discover a mismatch after a restart or
                // another client's model change. Restore only an empty source
                // field, never clobber text the user typed during that lookup.
                if self.state.selected_session == Some(session_id)
                    && state.input.read(cx).content().is_empty()
                {
                    let prompt = format!("/btw {}", state.question);
                    state
                        .input
                        .update(cx, |input, cx| input.set_content(prompt, cx));
                    if state.input == self.composer {
                        self.schedule_composer_draft_save(cx);
                    }
                }
            }
            Ok(Some(_)) => state.error = Some(tr!("btw.empty_answer")),
            Err(error) => state.error = Some(error.to_string()),
        }
        cx.notify();
    }

    pub(super) fn cancel_btw(&mut self, session_id: Uuid) {
        if let Some(state) = self.btw_states.get_mut(&session_id)
            && state.request.take().is_some()
        {
            state.error = Some(tr!("btw.cancelled"));
        }
    }

    pub(super) fn btw_panel_focus(&self) -> Option<FocusHandle> {
        self.state
            .selected_session
            .and_then(|id| self.btw_states.get(&id))
            .map(|state| state.focus.clone())
    }

    fn retry_btw(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        if self.state.selected_session != Some(session_id) {
            return;
        }
        let Some((question, input)) = self
            .btw_states
            .get(&session_id)
            .map(|state| (state.question.clone(), state.input.clone()))
        else {
            return;
        };
        if let Err(error) = self.ask_btw(question, input, cx) {
            self.show_toast(error);
            cx.notify();
        }
    }

    pub(super) fn render_btw_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(session_id) = self.state.selected_session else {
            return div().into_any_element();
        };
        let Some(state) = self.btw_states.get(&session_id) else {
            return div().into_any_element();
        };
        let theme = Theme::current(cx);
        let pending = state.request.is_some();
        let selection = state.selection.clone();
        selection.registry.borrow_mut().clear();
        let mut header = div()
            .flex_none()
            .px(px(16.0))
            .py(px(12.0))
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_start()
            .gap(px(8.0))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .max_h(px(120.0))
                    .overflow_hidden()
                    .text_size(ui_px(13.0))
                    .text_color(theme.text)
                    .child(state.question.clone()),
            );
        if !state.answer.is_empty() {
            header = header.child(
                icon_button("btw-copy", "icons/copy.svg", theme)
                    .track_focus(&state.copy_focus)
                    .tab_index(0)
                    .flex_none()
                    .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
                    .tooltip(Tooltip::text(tr!("btw.copy")))
                    .on_activation(cx, move |this, _, cx| {
                        if let Some(state) = this.btw_states.get(&session_id) {
                            cx.write_to_clipboard(ClipboardItem::new_string(state.answer.clone()));
                            this.show_toast(tr!("btw.copied"));
                            cx.notify();
                        }
                    }),
            );
        }
        let action = |label: String| {
            div()
                .id("btw-action")
                .track_focus(&state.action_focus)
                .tab_index(0)
                .min_h(px(30.0))
                .px(px(12.0))
                .rounded(px(6.0))
                .bg(theme.overlay)
                .hover(|style| style.bg(theme.overlay_strong))
                .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
                .flex()
                .items_center()
                .text_size(ui_px(12.0))
                .child(label)
        };
        let body = if pending {
            div()
                .size_full()
                .p(px(16.0))
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(motion::spin_slow(icon(
                            "icons/loader-circle.svg",
                            14.0,
                            theme.text_secondary,
                        )))
                        .child(tr!("btw.working")),
                )
                .child(div().flex().child(action(tr!("btw.cancel")).on_activation(
                    cx,
                    move |this, _, cx| {
                        this.cancel_btw(session_id);
                        cx.notify();
                    },
                )))
                .into_any_element()
        } else if let Some(error) = &state.error {
            div()
                .size_full()
                .p(px(16.0))
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(
                    div()
                        .text_color(theme.warning_text)
                        .child(tr!("btw.failed")),
                )
                .child(div().text_color(theme.text_secondary).child(error.clone()))
                .child(div().flex().child(action(tr!("btw.retry")).on_activation(
                    cx,
                    move |this, _, cx| {
                        this.retry_btw(session_id, cx);
                    },
                )))
                .into_any_element()
        } else {
            state
                .markdown
                .clone()
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element())
        };
        div()
            .id("btw-panel")
            .key_context("BtwPanel")
            .track_focus(&state.focus)
            .tab_index(0)
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_size(ui_px(13.0))
            .text_color(theme.text)
            .focus_visible(|style| style.border_1().border_color(theme.accent_focus))
            .on_action(cx.listener(|this, _: &DismissBtw, window, cx| {
                this.close_btw_panel(cx);
                window.focus(&this.composer.read(cx).focus(), cx);
            }))
            .child(header)
            .child(div().flex_1().min_h_0().min_w_0().child(body))
            .child(
                div()
                    .flex_none()
                    .px(px(16.0))
                    .py(px(10.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(ui_px(11.0))
                    .text_color(theme.text_tertiary)
                    .child(tr!("btw.transient")),
            )
            .when(!self.update_card_visible(), |element| {
                element.child(
                    canvas(
                        |_, _, _| (),
                        move |_, _, window, _| {
                            md::render::install_selection_input(window, &selection)
                        },
                    )
                    .absolute()
                    .size(px(0.0)),
                )
            })
            .into_any_element()
    }
}
