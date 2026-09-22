use super::*;

impl Fintwind {
    pub(super) fn finish_streaming_assistant(&mut self, session_id: Uuid) {
        if let Some(session) = self.state.session_mut(session_id) {
            for message in &mut session.messages {
                if message.role == MessageRole::Assistant && message.streaming {
                    message.streaming = false;
                }
            }
        }
    }

    pub(super) fn append_text_delta(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        delta: String,
    ) {
        let previous_phase = runtime.stream_phase;
        if previous_phase == Some(StreamPhase::Reasoning) {
            self.complete_reasoning_activity(session_id, runtime);
        }
        let continuing = previous_phase == Some(StreamPhase::Text);
        append_text_delta_to_session(&mut self.state.sessions, session_id, continuing, delta);
        self.state.mark_session_dirty(session_id);
        runtime.stream_phase = Some(StreamPhase::Text);
    }

    /// Reserve the assistant message for one text part (`session.text.started`).
    /// Tools that arrive before the part's batched tail then anchor after this
    /// message, so the tail fills the sentence instead of opening a new one.
    /// A redelivered start for a part that is already open is a no-op.
    fn open_text_fragment(&mut self, session_id: Uuid, runtime: &mut SessionRuntime, part: String) {
        if part.is_empty()
            || runtime.open_text.contains_key(&part)
            || runtime.settled_text.contains(&part)
        {
            return;
        }
        if runtime.stream_phase == Some(StreamPhase::Reasoning) {
            self.complete_reasoning_activity(session_id, runtime);
        }
        if runtime.stream_phase == Some(StreamPhase::Text) {
            self.finish_streaming_assistant(session_id);
        }
        let created = self.state.session_mut(session_id).is_some_and(|session| {
            open_keyed_text_part(
                session,
                &mut runtime.open_text,
                &mut runtime.settled_text,
                &part,
            )
        });
        if created {
            self.state.mark_session_dirty(session_id);
            runtime.stream_phase = Some(StreamPhase::Text);
        }
    }

    /// Route one text delta. An empty part is the pre-keying fallback and
    /// follows stream phase. A keyed delta of an open part appends there even
    /// after tool events, and does not move the phase back to text — the next
    /// tool must keep grouping with the block already under way.
    fn append_keyed_text_delta(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        part: String,
        delta: String,
    ) {
        if part.is_empty() {
            self.append_text_delta(session_id, runtime, delta);
            return;
        }
        if delta.is_empty() || runtime.settled_text.contains(&part) {
            return;
        }
        let already_open = runtime.open_text.contains_key(&part);
        if !already_open {
            if runtime.stream_phase == Some(StreamPhase::Reasoning) {
                self.complete_reasoning_activity(session_id, runtime);
            }
            if runtime.stream_phase == Some(StreamPhase::Text) {
                self.finish_streaming_assistant(session_id);
            }
        }
        let opened = {
            let Some(session) = self.state.session_mut(session_id) else {
                return;
            };
            bind_keyed_text_delta(
                session,
                &mut runtime.open_text,
                &runtime.settled_text,
                &part,
                &delta,
            )
        };
        if opened {
            runtime.stream_phase = Some(StreamPhase::Text);
        } else if already_open && self.state.selected_session == Some(session_id) {
            if let Some(message_id) = runtime.open_text.get(&part).copied()
                && let Some(index) = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .and_then(|session| {
                        session
                            .messages
                            .iter()
                            .position(|message| message.id == message_id)
                    })
            {
                self.remeasure_transcript_message(index);
            }
        }
        self.state.mark_session_dirty(session_id);
    }

    /// Close a text part with its authoritative text (`session.text.ended`).
    /// An empty text drops the reserved message and pulls later block anchors
    /// back over the hole. The phase becomes activity, matching a settled
    /// reasoning fragment, so the next tool does not look like a fresh stretch
    /// of text.
    fn complete_text_fragment(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        part: String,
        text: Option<String>,
    ) {
        if part.is_empty() {
            if runtime.stream_phase == Some(StreamPhase::Text) {
                self.finish_streaming_assistant(session_id);
                runtime.stream_phase = Some(StreamPhase::Activity);
            }
            return;
        }
        // A redelivered end must not open another copy of the sentence after
        // the tools, and must not move the phase back to text.
        if runtime.settled_text.contains(&part) {
            return;
        }
        let settled = if let Some(session) = self.state.session_mut(session_id) {
            let settled =
                settle_keyed_text_fragment(session, &mut runtime.open_text, &part, text.as_deref());
            session.updated_at = unix_time();
            settled
        } else {
            return;
        };
        runtime.settled_text.insert(part);
        runtime.stream_phase = Some(StreamPhase::Activity);
        if self.state.selected_session == Some(session_id)
            && let KeyedTextSettle::Rewritten(index) = settled
        {
            self.remeasure_transcript_message(index);
        }
    }

    fn complete_reasoning_activity(&mut self, session_id: Uuid, runtime: &SessionRuntime) {
        let bound = runtime.open_reasoning.values().copied().collect::<Vec<_>>();
        let completed_block = self
            .state
            .session_mut(session_id)
            .and_then(|session| complete_reasoning_activity_bound(session, &bound));
        // Live reasoning auto-collapses on a phase change and may already sit
        // outside the three-row streaming remeasure window.
        if self.state.selected_session == Some(session_id)
            && let Some(block_index) = completed_block
        {
            self.remeasure_transcript_block(block_index);
        }
    }

    /// A provider reasoning fragment opened (opencode `session.reasoning.started`).
    /// The fragment is the persisted part's identity, so its deltas route by
    /// key rather than transcript position — a provider can flush a tail delta
    /// after the next tool's events have already landed, and that tail belongs
    /// back inside its own thought, not in a stray new one.
    fn open_reasoning_fragment(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        part: String,
    ) {
        // Reopening a key abandons whatever the previous fragment under it
        // produced — and lifts any settle — so the next delta opens a fresh
        // block.
        runtime.open_reasoning.remove(&part);
        runtime.settled_reasoning.remove(&part);
        if runtime.stream_phase == Some(StreamPhase::Reasoning) {
            self.complete_reasoning_activity(session_id, runtime);
            // The opened fragment continues the same stretch of work (its
            // block grouping follows), but its first delta must start a new
            // activity instead of appending to the one just completed.
            runtime.stream_phase = Some(StreamPhase::Activity);
        }
    }

    pub(super) fn append_reasoning_delta(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        part: String,
        delta: String,
    ) {
        if part.is_empty() {
            self.append_unkeyed_reasoning_delta(session_id, runtime, delta);
            return;
        }
        let continuing_work = matches!(
            runtime.stream_phase,
            Some(StreamPhase::Reasoning | StreamPhase::Activity)
        );
        let opened = if let Some(session) = self.state.session_mut(session_id) {
            let opened = bind_keyed_reasoning_delta(
                session,
                &mut runtime.open_reasoning,
                &runtime.settled_reasoning,
                &part,
                &delta,
                continuing_work,
            );
            session.updated_at = unix_time();
            opened
        } else {
            return;
        };
        if opened {
            self.finish_streaming_assistant(session_id);
        }
        runtime.stream_phase = Some(StreamPhase::Reasoning);
    }

    /// A fragment settled (opencode `session.reasoning.ended`). Its durable
    /// `text` is what the stored reasoning part keeps, so it overwrites the
    /// accumulated deltas — healing anything lost or reordered. An empty text
    /// retires the fragment's block, mirroring the replay path that filters
    /// empty stored parts.
    fn complete_reasoning_fragment(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        part: String,
        text: Option<String>,
    ) {
        if part.is_empty() {
            if runtime.stream_phase == Some(StreamPhase::Reasoning) {
                self.complete_reasoning_activity(session_id, runtime);
                runtime.stream_phase = None;
            }
            return;
        }
        let continuing_work = matches!(
            runtime.stream_phase,
            Some(StreamPhase::Reasoning | StreamPhase::Activity)
        );
        let settled_block = if let Some(session) = self.state.session_mut(session_id) {
            let settled = settle_keyed_reasoning_fragment(
                session,
                &mut runtime.open_reasoning,
                &part,
                text.as_deref(),
                continuing_work,
            );
            session.updated_at = unix_time();
            settled
        } else {
            None
        };
        // The fragment is now complete on the provider side: any tail delta
        // still in flight for the key carries nothing the end event did not.
        runtime.settled_reasoning.insert(part);
        // Live reasoning auto-collapses on a phase change and may already sit
        // outside the three-row streaming remeasure window.
        if self.state.selected_session == Some(session_id)
            && let Some(block_index) = settled_block
        {
            self.remeasure_transcript_block(block_index);
        }
        // The turn stays mid-work after one fragment settles: the next
        // fragment's block must continue the same transcript group, exactly
        // as the replay path merges consecutive stored parts into one block.
        // Activity (not Reasoning) so an unkeyed delta cannot append into the
        // just-settled thought.
        runtime.stream_phase = Some(StreamPhase::Activity);
    }

    /// Pre-keying transports (older daemons on the wire) carry no fragment
    /// identity: deltas group by transcript phase exactly as they always did.
    fn append_unkeyed_reasoning_delta(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        delta: String,
    ) {
        let previous_phase = runtime.stream_phase;
        let continuing = previous_phase == Some(StreamPhase::Reasoning);
        if !continuing && delta.trim().is_empty() {
            return;
        }
        let now = unix_time_millis();
        if !continuing {
            self.finish_streaming_assistant(session_id);
        }
        if let Some(session) = self.state.session_mut(session_id) {
            if continuing
                && let Some(reasoning) = session
                    .transcript_blocks
                    .last_mut()
                    .and_then(|block| block.activities.last_mut())
                    .and_then(|activity| activity.reasoning.as_mut())
            {
                reasoning.content.push_str(&delta);
                reasoning.finished_at_ms = now;
            } else {
                push_transcript_activity(
                    session,
                    ActivityItem::from_reasoning(
                        ReasoningBlock {
                            content: delta,
                            started_at_ms: now,
                            finished_at_ms: now,
                        },
                        false,
                    ),
                    matches!(
                        previous_phase,
                        Some(StreamPhase::Reasoning | StreamPhase::Activity)
                    ),
                );
            }
            session.updated_at = unix_time();
        }
        runtime.stream_phase = Some(StreamPhase::Reasoning);
    }

    pub(super) fn update_activity(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        item: ActivityItem,
    ) {
        let previous_phase = runtime.stream_phase;
        if previous_phase == Some(StreamPhase::Text) {
            self.finish_streaming_assistant(session_id);
        }
        if previous_phase == Some(StreamPhase::Reasoning) {
            self.complete_reasoning_activity(session_id, runtime);
        }

        let continuing_work = matches!(
            previous_phase,
            Some(StreamPhase::Reasoning | StreamPhase::Activity)
        );
        if let Some(session) = self.state.session_mut(session_id) {
            for block in session.transcript_blocks.iter_mut().rev() {
                let matching = block.activities.iter_mut().rev().find(|activity| {
                    item.source_id
                        .as_ref()
                        .is_some_and(|id| activity.source_id.as_ref() == Some(id))
                        || (item.source_id.is_none()
                            && activity.title == item.title
                            && !activity.complete)
                });
                if let Some(activity) = matching {
                    let has_arguments = item.arguments.is_some();
                    let replaces_changes = !item.file_changes.is_empty();
                    let activity_id = activity.id;
                    activity.kind = item.kind;
                    activity.title = item.title;
                    activity.complete = item.complete;
                    activity.failed = item.failed;
                    if item.detail.is_some() {
                        activity.detail = item.detail;
                    }
                    if item.arguments.is_some() {
                        activity.arguments = item.arguments;
                    }
                    if item.output.is_some() {
                        activity.output = item.output;
                    }
                    if !item.image_urls.is_empty() {
                        activity.image_urls = item.image_urls;
                    }
                    if !item.file_changes.is_empty() {
                        activity.file_changes = item.file_changes;
                    }
                    if item.display_target.is_some() {
                        activity.display_target = item.display_target;
                    }
                    if item.display_description.is_some()
                        && (activity.display_description.is_none() || has_arguments)
                    {
                        activity.display_description = item.display_description;
                    }
                    if item.reasoning.is_some() {
                        activity.reasoning = item.reasoning;
                    }
                    session.updated_at = unix_time();
                    runtime.stream_phase = Some(StreamPhase::Activity);
                    if replaces_changes {
                        // The rows this activity's diff was built from are gone;
                        // an expanded card rebuilds from the new ones.
                        self.activity_diffs.borrow_mut().remove(&activity_id);
                    }
                    return;
                }
            }

            push_transcript_activity(session, item, continuing_work);
            session.updated_at = unix_time();
        }
        runtime.stream_phase = Some(StreamPhase::Activity);
    }

    pub(super) fn complete_turn_blocks(&mut self, session_id: Uuid) {
        if let Some(session) = self.state.session_mut(session_id) {
            for block in &mut session.transcript_blocks {
                for activity in &mut block.activities {
                    activity.complete = true;
                }
            }
        }
        self.rebuild_todo_summary(session_id);
    }

    /// Drop assistant messages this turn reserved for a text part and never
    /// filled. They are not an answer, and leaving them in place would hide
    /// the fallback the turn shows when the provider produced no text.
    pub(super) fn drop_blank_assistant_messages(&mut self, session_id: Uuid) {
        let Some(turn_id) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(AgentSession::active_turn_id)
        else {
            return;
        };
        let Some(session) = self.state.session_mut(session_id) else {
            return;
        };
        let blank = session
            .messages
            .iter()
            .enumerate()
            .filter(|(_, message)| {
                message.role == MessageRole::Assistant
                    && message.turn_id == Some(turn_id)
                    && message.content.trim().is_empty()
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in blank.into_iter().rev() {
            remove_message_at(session, index);
        }
    }

    pub(super) fn turn_has_assistant_message(&self, session_id: Uuid) -> bool {
        self.state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| {
                let Some(turn_id) = session.active_turn_id() else {
                    return false;
                };
                session.messages.iter().any(|message| {
                    message.role == MessageRole::Assistant
                        && message.turn_id == Some(turn_id)
                        && !message.content.trim().is_empty()
                })
            })
    }

    pub(super) fn accepts_turn_output(&self, session_id: Uuid) -> bool {
        // The turn begins at submission accept, before its prompt has reached
        // any provider. While preparation is still running, a reused runtime
        // could only be draining leftovers of a settled turn — output landing
        // in the new turn then would attribute stale text to it.
        !self.submission_preparations.contains(&session_id)
            && self
                .state
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| {
                    session.active_turn_id().is_some()
                        && matches!(
                            session.status,
                            SessionStatus::Connecting
                                | SessionStatus::Working
                                | SessionStatus::Waiting
                        )
                })
    }

    /// Returns whether the runtime should remain attached after this event.
    ///
    /// `allow_queue_drain` is false when the caller is flushing buffered
    /// events for a turn the user just stopped: a settling event must not
    /// start queued follow-ups then, because the user asked to stop, not to
    /// continue.
    pub(super) fn handle_driver_event(
        &mut self,
        session_id: Uuid,
        runtime: &mut SessionRuntime,
        event: DriverEvent,
        allow_queue_drain: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        runtime.last_active_at = Instant::now();
        match event {
            DriverEvent::RuntimeEventCursorAdvanced(cursor) => {
                if let Some(session) = self.state.session_mut(session_id) {
                    session.runtime_event_cursor = Some(cursor);
                }
            }
            DriverEvent::Connected { provider_cursor } => {
                runtime.last_driver_error = None;
                runtime.last_background_refresh_at = Instant::now();
                // A fresh attach knows nothing about a backoff that predated it;
                // the next status report, if any, says whether one is running.
                self.provider_retries.remove(&session_id);
                runtime.driver.refresh_background_work();
                if let Some(session) = self.state.session_mut(session_id) {
                    session.provider_cursor = provider_cursor;
                    // List-level mirror of the native id, so reconciliation
                    // can match this session's row without hydrating it.
                    session.native_session_id = session
                        .provider_cursor
                        .as_ref()
                        .map(|cursor| cursor.native_id().to_owned());
                    if session.status == SessionStatus::Connecting {
                        session.status = SessionStatus::Working;
                    }
                }
            }
            DriverEvent::NativeSessionsChanged => {
                // Sessions came or went on the OpenCode server while a driver
                // is attached; refresh the sidebar's roster.
                self.schedule_native_session_reconcile(cx);
            }
            DriverEvent::AgentPresetSelected(agent_preset) => {
                if let Some(session) = self.state.session_mut(session_id) {
                    session.agent_preset = agent_preset;
                }
            }
            DriverEvent::AutoTitleUpdated(title) => {
                if let Some(session) = self.state.session_mut(session_id) {
                    session.set_auto_title(title);
                }
            }
            DriverEvent::AvailableCommands(names) => {
                if let Some(session) = self
                    .state
                    .session_mut(session_id)
                    .filter(|session| session.available_commands != names)
                {
                    session.available_commands = names;
                    // The drain has no `Context`; the frame loop rebuilds the
                    // drawn index when it sees this.
                    self.composer_sources_stale = true;
                }
            }
            DriverEvent::TurnStarted => {
                runtime.last_driver_error = None;
                runtime.provider_phase = None;
                self.provider_retries.remove(&session_id);
                if let Some(session) = self.state.session_mut(session_id) {
                    session.resume_provider_turn();
                }
            }
            DriverEvent::ProviderBusy => {
                // A call is in flight again, so the backoff it replaced is over
                // whether or not the provider retracted it.
                self.provider_retries.remove(&session_id);
                if self.accepts_turn_output(session_id) {
                    runtime.provider_phase = Some(ProviderPhase::Responding { since: unix_time() });
                }
            }
            DriverEvent::ProviderRetry {
                attempt,
                message,
                action,
                next_at_ms,
            } => {
                // Deliberately not gated on `accepts_turn_output`: a retry is
                // provider state, not turn output. It keeps arriving after the
                // turn settled or failed, and dropping those reports is what
                // left the reader watching a frozen transcript while the
                // provider was visibly retrying.
                runtime.provider_phase = None;
                let retry = ProviderRetry {
                    attempt,
                    message,
                    action,
                    next_at_ms,
                    received_at_ms: unix_time_millis(),
                };
                // One expiry sleeper per backoff episode: repeated attempt
                // reports for the same episode reuse the first one.
                let new_episode = self.provider_retries.insert(session_id, retry).is_none();
                if new_episode {
                    self.schedule_provider_retry_expiry(session_id, cx);
                }
            }
            DriverEvent::TextStarted { part } => {
                self.provider_retries.remove(&session_id);
                if self.accepts_turn_output(session_id) {
                    runtime.provider_phase = None;
                    self.open_text_fragment(session_id, runtime, part);
                }
            }
            DriverEvent::TextDelta { part, delta } => {
                self.provider_retries.remove(&session_id);
                if self.accepts_turn_output(session_id) {
                    runtime.provider_phase = None;
                    self.append_keyed_text_delta(session_id, runtime, part, delta);
                }
            }
            DriverEvent::TextEnded { part, text } => {
                self.provider_retries.remove(&session_id);
                if self.accepts_turn_output(session_id) {
                    runtime.provider_phase = None;
                    self.complete_text_fragment(session_id, runtime, part, text);
                }
            }
            DriverEvent::ReasoningStarted { part } => {
                self.provider_retries.remove(&session_id);
                if self.accepts_turn_output(session_id) {
                    runtime.provider_phase = None;
                    self.open_reasoning_fragment(session_id, runtime, part);
                }
            }
            DriverEvent::ReasoningDelta { part, delta } => {
                self.provider_retries.remove(&session_id);
                if self.accepts_turn_output(session_id) {
                    runtime.provider_phase = None;
                    self.append_reasoning_delta(session_id, runtime, part, delta);
                }
            }
            DriverEvent::ReasoningEnded { part, text } => {
                self.provider_retries.remove(&session_id);
                if self.accepts_turn_output(session_id) {
                    runtime.provider_phase = None;
                    self.complete_reasoning_fragment(session_id, runtime, part, text);
                }
            }
            DriverEvent::Activity {
                id,
                kind,
                title,
                detail,
                complete,
            } => {
                self.provider_retries.remove(&session_id);
                if self.accepts_turn_output(session_id) {
                    let refresh_branch = should_refresh_branch_after_activity(kind, complete)
                        && self.state.selected_session == Some(session_id);
                    let item = ActivityItem::new(id, kind, title, detail, complete);
                    self.observe_foreground_command_activity(session_id, &item);
                    runtime.provider_phase = None;
                    self.update_activity(session_id, runtime, item);
                    if kind == ActivityKind::Plan {
                        self.rebuild_todo_summary(session_id);
                    }
                    if refresh_branch {
                        self.refresh_selected_branch_snapshot(cx);
                    }
                }
            }
            DriverEvent::RichActivity(item) => {
                self.provider_retries.remove(&session_id);
                if self.accepts_turn_output(session_id) {
                    let refresh_branch =
                        should_refresh_branch_after_activity(item.kind, item.complete)
                            && self.state.selected_session == Some(session_id);
                    self.observe_foreground_command_activity(session_id, &item);
                    runtime.provider_phase = None;
                    let plan_activity = item.kind == ActivityKind::Plan;
                    self.update_activity(session_id, runtime, item);
                    if plan_activity {
                        self.rebuild_todo_summary(session_id);
                    }
                    if refresh_branch {
                        self.refresh_selected_branch_snapshot(cx);
                    }
                }
            }
            DriverEvent::BackgroundWork(event) => {
                // Background work is session state, not turn output. It must
                // survive a settled or rewound turn and therefore bypasses
                // `accepts_turn_output` deliberately.
                self.handle_background_work_event(session_id, event);
            }
            DriverEvent::Permission {
                request_id,
                title,
                detail,
                options,
            } => {
                if self.accepts_turn_output(session_id) {
                    runtime.provider_phase = None;
                    runtime.pending_permission = Some(PendingPermission {
                        request_id,
                        title,
                        detail,
                        options,
                    });
                    if let Some(session) = self.state.session_mut(session_id) {
                        session.status = SessionStatus::Waiting;
                    }
                }
            }
            DriverEvent::UserInputRequested {
                request_id,
                questions,
            } => {
                if self.accepts_turn_output(session_id) && !questions.is_empty() {
                    runtime.provider_phase = None;
                    runtime.pending_user_input = Some(PendingUserInput::new(request_id, questions));
                    if self.state.selected_session == Some(session_id) {
                        self.user_input_answer
                            .update(cx, |input, cx| input.clear(cx));
                    }
                    if let Some(session) = self.state.session_mut(session_id) {
                        session.status = SessionStatus::Waiting;
                    }
                }
            }
            DriverEvent::SteerAccepted { message } => {
                let submission = runtime
                    .pending_steers
                    .iter()
                    .position(|submission| submission.provider_prompt() == message)
                    .and_then(|index| runtime.pending_steers.remove(index))
                    // Providers normally echo the exact transport text, but a
                    // normalized echo still acknowledges the oldest pending
                    // steer. Preserve its attachment presentation metadata.
                    .or_else(|| runtime.pending_steers.pop_front())
                    .unwrap_or_else(|| ComposerSubmission::plain(message.clone()));
                // The provider folded the message into the live turn. Append
                // it to the same turn so the transcript mirrors the provider
                // conversation (no new turn boundary).
                if let Some(session) = self.state.session_mut(session_id) {
                    session.push_user_message_with_presentation(
                        message,
                        submission.display_content,
                        submission.attachments,
                    );
                    session.updated_at = unix_time();
                }
            }
            DriverEvent::SteerRejected { message, reason } => {
                let submission = runtime
                    .pending_steers
                    .iter()
                    .position(|submission| submission.provider_prompt() == message)
                    .and_then(|index| runtime.pending_steers.remove(index))
                    .or_else(|| runtime.pending_steers.pop_front())
                    .unwrap_or_else(|| ComposerSubmission::plain(message));
                let (busy, settled_cleanly) = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .map(|session| {
                        let settled_cleanly = session
                            .turns
                            .last()
                            .is_some_and(|turn| turn.status == TurnStatus::Completed);
                        (session.is_busy(), settled_cleanly)
                    })
                    .unwrap_or((false, false));
                if busy {
                    self.enqueue_follow_up_submission(session_id, submission, cx);
                    if self.state.selected_session == Some(session_id) {
                        self.show_toast(tr!(
                            "session.steer_rejected",
                            error = compact_driver_error(&reason)
                        ));
                    }
                } else if settled_cleanly {
                    // The turn settled before the steer arrived; run the
                    // message as a fresh turn instead of losing it. Submission
                    // is deferred through the queue-drain pass because this
                    // session's runtime is detached from the map while its
                    // events are handled — an inline submit would spawn a
                    // second driver process only to have it clobbered when the
                    // drain re-inserts the detached runtime.
                    if let Some(session) = self.state.session_mut(session_id) {
                        session
                            .queued_messages
                            .insert(0, submission.into_queued_message());
                    }
                    if allow_queue_drain {
                        self.pending_queue_drains.push(session_id);
                    }
                } else {
                    // The user stopped the turn (or the provider died) before
                    // the steer landed. Keep the message visible and
                    // user-controlled instead of auto-running it.
                    self.enqueue_follow_up_submission(session_id, submission, cx);
                }
            }
            DriverEvent::TurnStatsUpdated(stats) => {
                // Turn meta, not turn output: it must survive the drain of a
                // turn that is settling, so it bypasses `accepts_turn_output`
                // the way usage does. It lands only while the turn is still
                // the active one — the driver emits it before the turn's
                // terminal event, and a turn that already settled keeps the
                // stats stored with it, so re-delivery changes nothing.
                if let Some(session) = self.state.session_mut(session_id)
                    && let Some(turn) = session
                        .turns
                        .last_mut()
                        .filter(|turn| turn.status == TurnStatus::Running)
                    && turn.stats.as_ref() != Some(&stats)
                {
                    turn.stats = Some(stats);
                    self.state.mark_session_dirty(session_id);
                }
            }
            DriverEvent::PlanUsageUpdated(_) => {
                // The account plan meters were retired from the usage panel;
                // the wire event stays for protocol stability and is ignored.
            }
            DriverEvent::UsageUpdated {
                context_tokens,
                context_window,
                session_total,
                cache_read,
                prompt_tokens,
            } => {
                // Meta about the conversation, not turn output: it applies
                // even while a rewound or cancelled turn's tail drains. Every
                // value is absolute, so re-delivery merges idempotently.
                if let Some(session) = self.state.session_mut(session_id) {
                    let usage = session.context_usage.get_or_insert(ContextUsage::default());
                    if let Some(tokens) = context_tokens {
                        usage.tokens = tokens;
                    }
                    if let Some(window) = context_window {
                        usage.window = Some(window);
                    }
                    if let Some(total) = session_total {
                        usage.total_tokens = Some(total);
                    }
                    if let Some(read) = cache_read {
                        usage.cache_read = Some(read);
                    }
                    if let Some(prompt) = prompt_tokens {
                        usage.prompt_tokens = Some(prompt);
                    }
                    self.state.mark_session_dirty(session_id);
                }
            }
            DriverEvent::CompactionUpdated(state) => {
                // Compaction is conversation plumbing, not turn output: it can
                // start, settle, or fail while no turn is live — the
                // provider's own automatic overflow compaction emits the same
                // events — so like usage it bypasses `accepts_turn_output`.
                // The row set moves here — the compacting row appears, and a
                // failure or withdrawal removes it — so the transition is
                // spliced against this snapshot rather than left to the
                // generic sync, whose shrink path resets the whole list.
                let previous_kinds = self.snapshot_selected_transcript_rows(session_id);
                let previous = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .and_then(|session| session.compaction.as_ref().map(|c| c.status));
                if let Some(session) = self.state.session_mut(session_id) {
                    let changed = session.compaction.as_ref() != Some(&state);
                    if changed {
                        session.compaction = Some(state.clone());
                    }
                    if state.status == CompactionStatus::Completed {
                        let before = session.messages.len();
                        upsert_compaction_transcript(session, &state);
                        if changed || session.messages.len() != before {
                            self.state.mark_session_dirty(session_id);
                        }
                    } else if changed {
                        self.state.mark_session_dirty(session_id);
                    }
                }
                if let Some(previous_kinds) = previous_kinds.as_deref() {
                    self.splice_active_transcript_rows_after_visibility_change(previous_kinds);
                }
                // Only transitions surface. Re-delivery and attach-time
                // seeding replay the stored state and must not re-toast an
                // outcome the user has already seen; a withdrawal (the user
                // or the provider aborted) just clears the indicator.
                if previous != Some(state.status) && self.state.selected_session == Some(session_id)
                {
                    match state.status {
                        CompactionStatus::Completed => {
                            self.show_toast(tr!("session.compaction_completed"));
                        }
                        CompactionStatus::Failed => {
                            self.show_toast(tr!("session.compaction_failed"));
                        }
                        CompactionStatus::Running | CompactionStatus::Cancelled => {}
                    }
                }
            }
            DriverEvent::TurnFinished { success, summary } => {
                self.settle_foreground_work(
                    session_id,
                    if success {
                        BackgroundWorkStatus::Completed
                    } else {
                        BackgroundWorkStatus::Failed
                    },
                );
                let previous_kinds = self.snapshot_selected_transcript_rows(session_id);
                runtime.last_driver_error = None;
                if self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .and_then(AgentSession::active_turn_id)
                    .is_none()
                {
                    return true;
                }
                let task_notification = cx.active_window().is_none().then(|| {
                    self.state
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .map(|session| {
                            let title = if session.display_title() == AgentSession::DEFAULT_TITLE {
                                tr!("session.new_task")
                            } else {
                                session.display_title().to_owned()
                            };
                            let body = if success {
                                tr!("session.turn_completed")
                            } else {
                                tr!("session.stopped")
                            };
                            (title, body)
                        })
                });
                self.finish_streaming_assistant(session_id);
                self.complete_turn_blocks(session_id);
                runtime.stream_phase = None;
                runtime.open_reasoning.clear();
                runtime.settled_reasoning.clear();
                runtime.open_text.clear();
                runtime.settled_text.clear();
                runtime.provider_phase = None;
                self.drop_blank_assistant_messages(session_id);
                let needs_fallback = !self.turn_has_assistant_message(session_id);
                if let Some(session) = self.state.session_mut(session_id) {
                    session.status = if success {
                        SessionStatus::Idle
                    } else {
                        SessionStatus::Failed
                    };
                    if needs_fallback {
                        session.push_message(
                            MessageRole::Assistant,
                            summary.unwrap_or_else(|| {
                                if success {
                                    tr!("session.turn_completed")
                                } else {
                                    tr!("session.stopped_before_response")
                                }
                            }),
                        );
                    }
                }
                self.finish_active_turn(
                    session_id,
                    if success {
                        TurnStatus::Completed
                    } else {
                        TurnStatus::Failed
                    },
                );
                runtime.pending_permission = None;
                runtime.pending_user_input = None;
                // The agent may have edited files or switched branches, so the
                // cached view of the workspace is no longer trustworthy. This
                // handler has no `Context`, so the drain loop acts on the flag.
                if self.state.selected_session == Some(session_id) {
                    self.workspace_queries_stale = true;
                }
                runtime.driver.refresh_background_work();
                self.capture_latest_turn_checkpoint_for(session_id);
                if allow_queue_drain && success {
                    // Start the next queued follow-up once the runtime has
                    // been re-inserted so the same process is reused.
                    self.pending_queue_drains.push(session_id);
                }
                if let Some(previous_kinds) = previous_kinds.as_deref() {
                    self.splice_active_transcript_rows_after_visibility_change(previous_kinds);
                }
                if let Some(Some((title, body))) = task_notification {
                    crate::platform::show_task_notification(
                        &task_notification_tag(session_id),
                        &title,
                        &body,
                        cx,
                    );
                }
            }
            DriverEvent::Error(error) => {
                // A fatal provider error retires the backoff: showing "Retrying"
                // beside the error that ended the attempt would contradict it.
                // A genuinely continuing backoff re-announces itself.
                let previous_kinds = self.snapshot_selected_transcript_rows(session_id);
                self.provider_retries.remove(&session_id);
                let error = compact_driver_error(&error);
                runtime.last_driver_error = Some(error.clone());
                if self.state.selected_session == Some(session_id) {
                    self.show_toast(error.clone());
                }
                let has_active_turn = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .and_then(AgentSession::active_turn_id)
                    .is_some();
                let not_working = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .is_some_and(|session| session.status != SessionStatus::Working);
                if not_working {
                    self.drop_blank_assistant_messages(session_id);
                }
                let should_append =
                    has_active_turn && not_working && !self.turn_has_assistant_message(session_id);
                if let Some(session) = self.state.session_mut(session_id)
                    && has_active_turn
                {
                    if session.status != SessionStatus::Working {
                        session.status = SessionStatus::Failed;
                    }
                    if should_append {
                        session.push_message(MessageRole::Assistant, error);
                    }
                }
                if let Some(previous_kinds) = previous_kinds.as_deref() {
                    self.splice_active_transcript_rows_after_visibility_change(previous_kinds);
                }
            }
            DriverEvent::ProcessExited => {
                self.mark_background_work_lost(session_id);
                let previous_kinds = self.snapshot_selected_transcript_rows(session_id);
                self.finish_streaming_assistant(session_id);
                self.complete_turn_blocks(session_id);
                runtime.stream_phase = None;
                runtime.open_reasoning.clear();
                runtime.settled_reasoning.clear();
                runtime.open_text.clear();
                runtime.settled_text.clear();
                runtime.provider_phase = None;
                // A dead provider process is not retrying.
                self.provider_retries.remove(&session_id);
                runtime.pending_permission = None;
                runtime.pending_user_input = None;
                self.drop_blank_assistant_messages(session_id);
                let needs_fallback = !self.turn_has_assistant_message(session_id);
                let failure_message = runtime
                    .last_driver_error
                    .take()
                    .unwrap_or_else(|| tr!("session.codex_exited_before_response"));
                let should_finish_turn = if let Some(session) = self.state.session_mut(session_id)
                    && matches!(
                        session.status,
                        SessionStatus::Connecting | SessionStatus::Working | SessionStatus::Waiting
                    ) {
                    session.status = SessionStatus::Failed;
                    session.updated_at = unix_time();
                    if needs_fallback {
                        session.push_message(MessageRole::Assistant, failure_message);
                    }
                    true
                } else {
                    false
                };
                let finished_turn = should_finish_turn
                    && self
                        .finish_active_turn(session_id, TurnStatus::Failed)
                        .is_some();
                if finished_turn {
                    self.capture_latest_turn_checkpoint_for(session_id);
                }
                if let Some(previous_kinds) = previous_kinds.as_deref() {
                    self.splice_active_transcript_rows_after_visibility_change(previous_kinds);
                }
                return false;
            }
        }
        true
    }

    /// Wake the app when a provider backoff's grace period runs out, so the
    /// card retires even if nothing else repaints.
    ///
    /// The card's only motion is a pulse dot, and `motion::pulse` schedules no
    /// frame at all under reduce-motion — exactly the still, already-settled
    /// transcript a retry tends to happen in. Without this sleeper the
    /// countdown and the expiry check would both depend on unrelated redraws
    /// that may never come.
    ///
    /// One sleeper per backoff episode, so an hours-long backoff still wakes
    /// only a handful of times.
    pub(super) fn schedule_provider_retry_expiry(&self, session_id: Uuid, cx: &mut Context<Self>) {
        // Cap each sleep so a bogus far-future stamp cannot park one sleeper
        // for years; a still-live report simply pays another tick.
        const MAX_SLEEP: Duration = Duration::from_secs(30);
        cx.spawn(async move |this, cx| {
            loop {
                let live = this
                    .update(cx, |this, _| {
                        this.provider_retries
                            .get(&session_id)
                            .is_some_and(|retry| retry.is_live(unix_time_millis()))
                    })
                    .unwrap_or(false);
                if !live {
                    break;
                }
                cx.background_executor().timer(MAX_SLEEP).await;
            }
            let _ = this.update(cx, |this, cx| {
                // A fresh report may have replaced the one this sleeper
                // watched; it carries its own sleeper, so leave it alone.
                let retire = this
                    .provider_retries
                    .get(&session_id)
                    .is_some_and(|retry| !retry.is_live(unix_time_millis()));
                if !retire {
                    return;
                }
                // Removing the tail row shrinks the list; splice it so the
                // reader's place survives instead of a full reset.
                let previous_kinds = this.snapshot_selected_transcript_rows(session_id);
                this.provider_retries.remove(&session_id);
                if let Some(previous_kinds) = previous_kinds.as_deref() {
                    this.splice_active_transcript_rows_after_visibility_change(previous_kinds);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Complete the reasoning activity a phase change settles. Fragments still
/// bound on the provider side win over transcript position — with keyed
/// fragments, "latest" and "open" can differ when a tail delta trails a tool.
pub(super) fn complete_reasoning_activity_bound(
    session: &mut AgentSession,
    bound: &[Uuid],
) -> Option<usize> {
    let mut latest_incomplete = None;
    let mut latest_bound = None;
    for (block_index, block) in session.transcript_blocks.iter_mut().enumerate().rev() {
        for (activity_index, activity) in block.activities.iter_mut().enumerate().rev() {
            if activity.reasoning.is_some() && !activity.complete {
                if latest_incomplete.is_none() {
                    latest_incomplete = Some((block_index, activity_index));
                }
                if bound.contains(&activity.id) {
                    latest_bound = Some((block_index, activity_index));
                    break;
                }
            }
        }
        if latest_bound.is_some() {
            break;
        }
    }
    let (block_index, activity_index) = latest_bound.or(latest_incomplete)?;
    let activity = session
        .transcript_blocks
        .get_mut(block_index)?
        .activities
        .get_mut(activity_index)?;
    activity.complete = true;
    session.updated_at = unix_time();
    Some(block_index)
}

/// The block and activity a reasoning fragment is bound to, located by the
/// activity id the fragment registry recorded when the block opened.
fn find_activity_mut(
    session: &mut AgentSession,
    activity_id: Uuid,
) -> Option<(usize, &mut ActivityItem)> {
    session
        .transcript_blocks
        .iter_mut()
        .enumerate()
        .find_map(|(block_index, block)| {
            block
                .activities
                .iter_mut()
                .find(|activity| activity.id == activity_id)
                .map(|activity| (block_index, activity))
        })
}

/// What [`settle_keyed_text_fragment`] did to the reserved message.
pub(super) enum KeyedTextSettle {
    /// No message was bound. An authoritative body was materialized at the
    /// current end when one was present.
    Missing,
    /// The bound message now holds the settled text.
    Rewritten(usize),
    /// An empty part was dropped, and later block anchors were pulled back.
    Removed,
}

/// Reserve an assistant message for `part` at the current end of the
/// transcript. A part that is already open is left alone, so a redelivered
/// `started` does not split the sentence. Returns whether a message was
/// created.
pub(super) fn open_keyed_text_part(
    session: &mut AgentSession,
    open_text: &mut HashMap<String, Uuid>,
    settled: &mut HashSet<String>,
    part: &str,
) -> bool {
    if part.is_empty() || open_text.contains_key(part) || settled.contains(part) {
        return false;
    }
    let id = push_streaming_assistant(session, String::new(), true);
    open_text.insert(part.to_owned(), id);
    true
}

/// Append `delta` to the message `part` already owns. Returns whether a new
/// message had to be opened (the `started` event never arrived). A rejoin
/// returns false: the caller must not move the stream phase back to text,
/// or the next tool would break out of the activity block already open.
pub(super) fn bind_keyed_text_delta(
    session: &mut AgentSession,
    open_text: &mut HashMap<String, Uuid>,
    settled: &HashSet<String>,
    part: &str,
    delta: &str,
) -> bool {
    if part.is_empty() || delta.is_empty() || settled.contains(part) {
        return false;
    }
    if let Some(message_id) = open_text.get(part).copied()
        && let Some(message) = session
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
    {
        message.content.push_str(delta);
        message.streaming = true;
        session.updated_at = unix_time();
        return false;
    }
    let id = push_streaming_assistant(session, delta.to_owned(), true);
    open_text.insert(part.to_owned(), id);
    true
}

/// Close `part` with the text the stored part keeps. An empty body removes
/// the reserved message. A part that never opened is materialized from a
/// non-empty body so the live view still carries it.
pub(super) fn settle_keyed_text_fragment(
    session: &mut AgentSession,
    open_text: &mut HashMap<String, Uuid>,
    part: &str,
    text: Option<&str>,
) -> KeyedTextSettle {
    let Some(message_id) = open_text.remove(part) else {
        if let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) {
            push_streaming_assistant(session, text.to_owned(), false);
        }
        return KeyedTextSettle::Missing;
    };
    let Some(index) = session
        .messages
        .iter()
        .position(|message| message.id == message_id)
    else {
        return KeyedTextSettle::Missing;
    };
    let empty = text.is_some_and(|text| text.trim().is_empty())
        || (text.is_none() && session.messages[index].content.trim().is_empty());
    if empty {
        remove_message_at(session, index);
        return KeyedTextSettle::Removed;
    }
    if let Some(text) = text {
        session.messages[index].content = text.to_owned();
    }
    session.messages[index].streaming = false;
    session.updated_at = unix_time();
    KeyedTextSettle::Rewritten(index)
}

fn push_streaming_assistant(session: &mut AgentSession, content: String, streaming: bool) -> Uuid {
    let mut message = match session.active_turn_id() {
        Some(turn_id) => Message::new_for_turn(MessageRole::Assistant, content, turn_id),
        None => Message::new(MessageRole::Assistant, content),
    };
    message.streaming = streaming;
    let id = message.id;
    session.messages.push(message);
    session.updated_at = unix_time();
    id
}

/// Drop one message and pull block anchors that sat after it back by one, so
/// a reserved-then-empty text part does not leave tools pointing past the end.
fn remove_message_at(session: &mut AgentSession, index: usize) {
    if index >= session.messages.len() {
        return;
    }
    session.messages.remove(index);
    for block in &mut session.transcript_blocks {
        if block.after_message > index {
            block.after_message -= 1;
        }
    }
    session.updated_at = unix_time();
}

/// Route one keyed reasoning delta to the fragment's own activity. Deltas of
/// an open fragment append even after intervening tool events — the provider
/// flushes its buffered tail late, and that tail belongs inside its thought.
/// A delta trailing its own fragment's end event adds nothing (the end event
/// already wrote the authoritative text), and neither does an empty one.
/// Returns whether a fresh activity opened (text can no longer be streaming
/// then).
pub(super) fn bind_keyed_reasoning_delta(
    session: &mut AgentSession,
    open_reasoning: &mut HashMap<String, Uuid>,
    settled: &HashSet<String>,
    part: &str,
    delta: &str,
    continuing_work: bool,
) -> bool {
    if delta.is_empty() || settled.contains(part) {
        return false;
    }
    let now = unix_time_millis();
    if let Some(activity_id) = open_reasoning.get(part).copied()
        && let Some((_, activity)) = find_activity_mut(session, activity_id)
        && let Some(reasoning) = activity.reasoning.as_mut()
    {
        reasoning.content.push_str(delta);
        reasoning.finished_at_ms = now;
        return false;
    }
    let mut item = ActivityItem::from_reasoning(
        ReasoningBlock {
            content: delta.to_owned(),
            started_at_ms: now,
            finished_at_ms: now,
        },
        false,
    );
    item.source_id = Some(part.to_owned());
    let item_id = item.id;
    push_transcript_activity(session, item, continuing_work);
    open_reasoning.insert(part.to_owned(), item_id);
    true
}

/// Close a keyed reasoning fragment with its authoritative text — the exact
/// content the stored reasoning part keeps, so the live view converges with
/// what a restart will render. An empty text retires the fragment's block,
/// mirroring the replay path that filters empty stored parts. Returns the
/// block whose rows must be remeasured.
pub(super) fn settle_keyed_reasoning_fragment(
    session: &mut AgentSession,
    open_reasoning: &mut HashMap<String, Uuid>,
    part: &str,
    text: Option<&str>,
    continuing_work: bool,
) -> Option<usize> {
    let now = unix_time_millis();
    let located = open_reasoning
        .remove(part)
        .and_then(|activity_id| find_activity_mut(session, activity_id));
    if let Some((block_index, activity)) = located {
        if text.map(str::trim) == Some("") {
            let id = activity.id;
            remove_activity(session, block_index, id);
            // The block itself may be gone; its old index can no longer name
            // a row to remeasure.
            return None;
        } else {
            if let (Some(reasoning), Some(text)) = (activity.reasoning.as_mut(), text) {
                reasoning.content = text.to_owned();
                reasoning.finished_at_ms = now;
            }
            activity.complete = true;
        }
        return Some(block_index);
    }
    // No delta ever landed for this fragment. Materialize it from the
    // authoritative text so the live view carries the part the stored
    // transcript keeps.
    if let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) {
        let mut item = ActivityItem::from_reasoning(
            ReasoningBlock {
                content: text.to_owned(),
                started_at_ms: now,
                finished_at_ms: now,
            },
            true,
        );
        item.source_id = Some(part.to_owned());
        push_transcript_activity(session, item, continuing_work);
    }
    None
}

/// Drop one activity from its block, and the block itself when the drop leaves
/// it empty — the shape the replay path produces for the same input.
fn remove_activity(session: &mut AgentSession, block_index: usize, activity_id: Uuid) {
    let Some(block) = session.transcript_blocks.get_mut(block_index) else {
        return;
    };
    block
        .activities
        .retain(|activity| activity.id != activity_id);
    if block.activities.is_empty() {
        session.transcript_blocks.remove(block_index);
    }
}

/// A completed edit or shell command is the earliest provider-neutral point at
/// which its filesystem effects are stable enough to re-read. The actual Git
/// work remains behind the branch cache's background fetch.
pub(super) fn should_refresh_branch_after_activity(
    kind: crate::model::ActivityKind,
    complete: bool,
) -> bool {
    complete
        && matches!(
            kind,
            crate::model::ActivityKind::Command | crate::model::ActivityKind::FileChange
        )
}

pub(super) fn push_transcript_activity(
    session: &mut AgentSession,
    item: ActivityItem,
    continuing_work: bool,
) {
    let after_message = session.messages.len();
    let turn_id = session.active_turn_id();
    if continuing_work
        && let Some(block) = session.transcript_blocks.last_mut()
        && block.after_message == after_message
        && block.turn_id == turn_id
    {
        block.activities.push(item);
    } else {
        session.transcript_blocks.push(TranscriptBlock {
            after_message,
            turn_id,
            activities: vec![item],
        });
    }
}

pub(super) fn stream_delta_kind(event: &DriverEvent) -> Option<StreamDeltaKind> {
    match event {
        DriverEvent::TextDelta { .. } => Some(StreamDeltaKind::Text),
        DriverEvent::ReasoningDelta { .. } => Some(StreamDeltaKind::Reasoning),
        _ => None,
    }
}

/// The reasoning fragment key a queued delta belongs to. Adjacent deltas of
/// one key coalesce into one pump pass; a different key ends the run, because
/// each fragment's chunk is routed to its own block.
fn stream_delta_part(event: &DriverEvent) -> Option<&str> {
    match event {
        DriverEvent::TextDelta { part, .. } | DriverEvent::ReasoningDelta { part, .. } => {
            Some(part)
        }
        _ => None,
    }
}

pub(super) fn stream_delta_text(event: &DriverEvent, kind: StreamDeltaKind) -> Option<&str> {
    match (kind, event) {
        (StreamDeltaKind::Text, DriverEvent::TextDelta { delta: text, .. })
        | (StreamDeltaKind::Reasoning, DriverEvent::ReasoningDelta { delta: text, .. }) => {
            Some(text)
        }
        _ => None,
    }
}

pub(super) fn compact_driver_error(error: &str) -> String {
    const MAX_LINES: usize = 6;
    const MAX_CHARS: usize = 800;

    let lines = error.lines().collect::<Vec<_>>();
    let mut compact = lines
        .iter()
        .take(MAX_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > MAX_LINES {
        compact.push_str("\n…");
    }
    if compact.chars().count() > MAX_CHARS {
        compact = compact.chars().take(MAX_CHARS - 1).collect();
        compact.push('…');
    }
    compact
}

/// Coalesce every adjacent delta of one kind while retaining provider order.
/// Runtime cursors are acknowledgements rather than visible boundaries, so the
/// newest cursor follows the combined delta. The full text enters layout in
/// this pass; Markdown's paint-only veil provides the progressive dissolve.
pub(super) fn pop_stream_batch(
    events: &mut VecDeque<DriverEvent>,
    kind: StreamDeltaKind,
) -> Option<DriverEvent> {
    let mut chunk = String::new();
    let mut part: Option<String> = None;
    let mut latest_cursor = None;
    loop {
        let next = match events.front() {
            Some(DriverEvent::RuntimeEventCursorAdvanced(_)) => {
                latest_cursor = events.pop_front();
                continue;
            }
            Some(event) if stream_delta_text(event, kind).is_some() => {
                let next_part = stream_delta_part(event).map(str::to_owned);
                if part.is_some() && part != next_part {
                    break;
                }
                events.pop_front()
            }
            _ => break,
        };
        let Some(event) = next else {
            break;
        };
        part = stream_delta_part(&event).map(str::to_owned);
        match (kind, event) {
            (StreamDeltaKind::Text, DriverEvent::TextDelta { delta: text, .. })
            | (StreamDeltaKind::Reasoning, DriverEvent::ReasoningDelta { delta: text, .. }) => {
                chunk.push_str(&text);
            }
            _ => unreachable!("the stream kind was checked before removing the event"),
        }
    }
    if let Some(cursor) = latest_cursor {
        events.push_front(cursor);
    }
    match kind {
        StreamDeltaKind::Text => Some(DriverEvent::TextDelta {
            part: part.unwrap_or_default(),
            delta: chunk,
        }),
        StreamDeltaKind::Reasoning => Some(DriverEvent::ReasoningDelta {
            part: part.unwrap_or_default(),
            delta: chunk,
        }),
    }
}

pub(super) fn append_text_delta_to_session(
    sessions: &mut [AgentSession],
    session_id: Uuid,
    continuing: bool,
    delta: String,
) {
    let Some(session) = sessions.iter_mut().find(|session| session.id == session_id) else {
        return;
    };
    if !continuing {
        for message in &mut session.messages {
            if message.role == MessageRole::Assistant && message.streaming {
                message.streaming = false;
            }
        }
    }
    let existing = continuing.then(|| {
        session
            .messages
            .iter_mut()
            .rev()
            .find(|message| message.role == MessageRole::Assistant && message.streaming)
    });
    if let Some(Some(message)) = existing {
        message.content.push_str(&delta);
    } else {
        let mut message = session
            .active_turn_id()
            .map(|turn_id| Message::new_for_turn(MessageRole::Assistant, delta.clone(), turn_id))
            .unwrap_or_else(|| Message::new(MessageRole::Assistant, delta));
        message.streaming = true;
        session.messages.push(message);
    }
    session.updated_at = unix_time();
}

/// Insert the provider's compacted summary into the transcript once, matching
/// the TUI's Compaction divider. Re-delivery and attach-time seeding reuse the
/// same snapshot, so an identical body is a no-op.
pub(super) fn upsert_compaction_transcript(session: &mut AgentSession, state: &CompactionState) {
    let Some(summary) = state
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    else {
        return;
    };
    if session
        .messages
        .iter()
        .any(|message| message.role == MessageRole::Compaction && message.content.trim() == summary)
    {
        return;
    }
    // A compaction is a conversation-level divider, not part of the live
    // turn: attaching it would fold the summary behind "Worked for N".
    session
        .messages
        .push(Message::new(MessageRole::Compaction, summary));
    session.updated_at = unix_time();
}
