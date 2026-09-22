use super::composer::{
    ComposerSubmitAction, composer_submit_action, dropped_file_mention, next_picker_highlight,
    submission_text, visible_branch_entries,
};
use super::runtime::merge_remote_session_catalog;
use super::settings::visible_settings_pages;
use super::{
    ESCAPE_STOP_CONFIRMATION_TIMEOUT, EscapeStopConfirmation, EscapeStopPress, EscapeStopTarget,
    NAVIGATION_RAIL_TICK_HEIGHT, NAVIGATION_RAIL_TURN_HEIGHT, PendingUserInput, SessionNavigation,
    StreamDeltaKind, TranscriptRowKind::*, active_navigation_turn_index,
    append_text_delta_to_session, assistant_response_footer, assistant_response_footer_index,
    assistant_response_footer_time, bind_keyed_reasoning_delta, bind_keyed_text_delta,
    changed_files_inline_message_index, compact_driver_error, complete_reasoning_activity_bound,
    disclosure_leading_space, fenced_code, fitted_file_tree_width, fitted_panel_widths,
    folded_transcript_row_kinds, folded_transcript_row_kinds_with_retry,
    format_turn_stats_duration, format_worked_duration, format_working_elapsed,
    maintain_transcript_anchor, message_opens_turn, message_starts_followup_turn,
    navigation_preview_snippet, navigation_rail_fade_visibility, navigation_rail_height,
    navigation_rail_scale, open_keyed_text_part, paused_toast_duration, pop_stream_batch,
    push_transcript_activity, session_is_reapable, settle_keyed_reasoning_fragment,
    settle_keyed_text_fragment, should_refresh_branch_after_activity, should_show_navigation_rail,
    should_show_scroll_to_bottom, task_id_from_notification_tag, task_notification_tag,
    transcript_anchor_end_space, transcript_navigation_turns, transcript_rests_at_tail,
    transcript_row_kinds, transcript_row_splice, transcript_rows_fingerprint,
    transcript_rows_fingerprint_with_retry, turn_stats_line, turn_tokens_per_second,
    upsert_compaction_transcript, widened_panel_width_for_file_editor,
    widened_panel_width_for_review,
};
use crate::git_branch::BranchEntry;
use crate::model::{
    ActivityItem, ActivityKind, AgentSession, Checkpoint, CheckpointFile, CheckpointStatus,
    CompactionState, CompactionStatus, DriverEvent, Message, MessageRole, ReasoningBlock,
    RuntimeEventCursor, SessionStatus, TranscriptBlock, TurnStats, TurnStatus, UserInputOption,
    UserInputQuestion,
};

#[test]
fn structured_user_input_preserves_question_order_and_custom_answer_precedence() {
    let questions = vec![
        UserInputQuestion {
            id: "environment".into(),
            header: "Environment".into(),
            question: "Where should this deploy?".into(),
            options: vec![UserInputOption {
                label: "Preview".into(),
                description: None,
            }],
            multi_select: false,
        },
        UserInputQuestion {
            id: "notes".into(),
            header: "Notes".into(),
            question: "Anything else?".into(),
            options: Vec::new(),
            multi_select: false,
        },
    ];
    let mut pending = PendingUserInput::new("request-1".into(), questions);
    pending
        .selections
        .insert("environment".into(), vec!["Preview".into()]);
    pending
        .selections
        .insert("notes".into(), vec!["stale choice".into()]);
    pending
        .custom_answers
        .insert("notes".into(), "Use the EU region".into());

    let answers = pending.answers();
    assert_eq!(answers[0].question_id, "environment");
    assert_eq!(answers[0].answers, ["Preview"]);
    assert_eq!(answers[1].question_id, "notes");
    assert_eq!(answers[1].answers, ["Use the EU region"]);
}
use gpui::{ListAlignment, ListState, Pixels, px};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};
use uuid::Uuid;

fn attach_changed_files(session: &mut AgentSession, files: Vec<CheckpointFile>) {
    let turn = session.turns.last_mut().expect("the test has a turn");
    turn.checkpoint = Some(Checkpoint {
        turn_count: turn.turn_count,
        git_ref: format!("refs/fintwind/test-turn-{}", turn.turn_count),
        status: CheckpointStatus::Ready,
        files,
        additions: 0,
        deletions: 0,
        created_at: 1,
    });
    turn.checkpoint
        .as_mut()
        .expect("checkpoint was just attached")
        .refresh_totals();
}

#[test]
fn remote_task_catalog_adds_web_tasks_without_replacing_hydrated_detail() {
    let project_id = Uuid::new_v4();
    let mut local = AgentSession::new(project_id);
    local.title = "Local title".into();
    local
        .messages
        .push(Message::new(MessageRole::User, "keep this transcript"));
    let local_id = local.id;

    let mut local_projection = local.list_projection();
    local_projection.title = "Renamed elsewhere".into();
    local_projection.status = SessionStatus::Waiting;
    local_projection.updated_at += 10;

    let mut web_task = AgentSession::new(project_id).list_projection();
    web_task.title = "Created in Web".into();
    let web_task_id = web_task.id;

    let mut catalog = vec![local];
    let removed =
        merge_remote_session_catalog(&mut catalog, vec![local_projection, web_task], |_| false);

    assert!(removed.is_empty());
    assert_eq!(catalog.len(), 2);
    let merged_local = catalog
        .iter()
        .find(|session| session.id == local_id)
        .unwrap();
    assert_eq!(merged_local.title, "Renamed elsewhere");
    assert_eq!(merged_local.status, SessionStatus::Waiting);
    assert_eq!(merged_local.messages.len(), 1);
    assert_eq!(merged_local.messages[0].content, "keep this transcript");
    assert!(catalog.iter().any(|session| session.id == web_task_id));
}

#[test]
fn composer_only_offers_stop_after_submission_preparation() {
    assert_eq!(
        composer_submit_action(Some(SessionStatus::Idle), false),
        ComposerSubmitAction::Send
    );
    assert_eq!(
        composer_submit_action(Some(SessionStatus::Connecting), true),
        ComposerSubmitAction::Preparing
    );
    assert_eq!(
        composer_submit_action(Some(SessionStatus::Connecting), false),
        ComposerSubmitAction::Stop
    );
    assert_eq!(
        composer_submit_action(Some(SessionStatus::Working), false),
        ComposerSubmitAction::Stop
    );
    assert_eq!(
        composer_submit_action(Some(SessionStatus::Failed), false),
        ComposerSubmitAction::Send
    );
}

#[test]
fn completed_mutating_activities_refresh_git_status() {
    assert!(should_refresh_branch_after_activity(
        ActivityKind::FileChange,
        true
    ));
    assert!(should_refresh_branch_after_activity(
        ActivityKind::Command,
        true
    ));
    assert!(!should_refresh_branch_after_activity(
        ActivityKind::FileChange,
        false
    ));
    assert!(!should_refresh_branch_after_activity(
        ActivityKind::FileRead,
        true
    ));
}

#[test]
fn escape_stop_requires_a_matching_second_press_and_expires() {
    let target = EscapeStopTarget {
        session_id: Uuid::new_v4(),
        turn_id: Some(Uuid::new_v4()),
    };
    let other_turn = EscapeStopTarget {
        session_id: target.session_id,
        turn_id: Some(Uuid::new_v4()),
    };
    let mut confirmation = EscapeStopConfirmation::default();
    let now = Instant::now();

    let first_arm = match confirmation.press(target, now) {
        EscapeStopPress::Arm(arm) => arm,
        EscapeStopPress::Stop => panic!("the first press must arm Stop"),
    };
    assert!(confirmation.is_armed_for(target, now + Duration::from_secs(2)));
    assert_eq!(
        confirmation.press(target, now + Duration::from_secs(2)),
        EscapeStopPress::Stop
    );
    assert!(!confirmation.is_armed_for(target, now + Duration::from_secs(2)));

    assert_eq!(ESCAPE_STOP_CONFIRMATION_TIMEOUT, Duration::from_secs(3));
    let second_arm = match confirmation.press(target, now) {
        EscapeStopPress::Arm(arm) => arm,
        EscapeStopPress::Stop => panic!("an unarmed confirmation must arm Stop"),
    };
    assert!(!confirmation.is_armed_for(target, now + Duration::from_secs(3)));
    let replacement_arm = match confirmation.press(other_turn, now + Duration::from_secs(3)) {
        EscapeStopPress::Arm(arm) => arm,
        EscapeStopPress::Stop => panic!("an expired or different target must arm Stop again"),
    };
    assert!(!confirmation.expire(first_arm));
    assert!(!confirmation.expire(second_arm));
    assert!(confirmation.expire(replacement_arm));
}

#[test]
fn toast_pause_preserves_time_with_a_readable_minimum() {
    assert_eq!(
        paused_toast_duration(Duration::from_secs(10), Duration::from_secs(3)),
        Duration::from_secs(7)
    );
    assert_eq!(
        paused_toast_duration(Duration::from_secs(1), Duration::from_secs(5)),
        Duration::from_millis(800)
    );
}

#[test]
fn dropped_files_mention_project_relative_paths() {
    let root = std::path::Path::new("/work/repo");
    assert_eq!(
        dropped_file_mention(
            Some(root),
            std::path::Path::new("/work/repo/src/main.rs"),
            false
        ),
        "src/main.rs"
    );
    assert_eq!(
        dropped_file_mention(Some(root), std::path::Path::new("/tmp/shot.png"), false),
        "/tmp/shot.png"
    );
    assert_eq!(
        dropped_file_mention(Some(root), std::path::Path::new("/work/repo/src"), true),
        "src/"
    );
    // The project root itself relativizes to nothing; keep it absolute.
    assert_eq!(
        dropped_file_mention(Some(root), std::path::Path::new("/work/repo"), true),
        "/work/repo/"
    );
    assert_eq!(
        dropped_file_mention(None, std::path::Path::new("/tmp/no project.png"), false),
        "/tmp/no project.png"
    );
}

#[test]
fn submissions_keep_typed_text_and_allow_attachment_only() {
    assert_eq!(submission_text("fix this", 2).as_deref(), Some("fix this"));
    // Chips travel on the files channel. An attachment-only send stays empty
    // text rather than inventing an `@` path.
    assert_eq!(submission_text("  ", 2).as_deref(), Some(""));
    assert_eq!(submission_text(" plain ", 0).as_deref(), Some("plain"));
    assert_eq!(submission_text("   ", 0), None);
}

#[test]
fn branch_picker_pins_selection_and_filters_by_name() {
    let branches = vec![
        BranchEntry {
            name: "topic/zebra".into(),
            checked_out_elsewhere: false,
        },
        BranchEntry {
            name: "main".into(),
            checked_out_elsewhere: false,
        },
        BranchEntry {
            name: "topic/apple".into(),
            checked_out_elsewhere: true,
        },
    ];
    assert_eq!(
        visible_branch_entries(&branches, "main", "")
            .iter()
            .map(|branch| branch.name.as_str())
            .collect::<Vec<_>>(),
        vec!["main", "topic/apple", "topic/zebra"]
    );
    assert_eq!(
        visible_branch_entries(&branches, "main", "TOPIC APPLE")
            .iter()
            .map(|branch| branch.name.as_str())
            .collect::<Vec<_>>(),
        vec!["topic/apple"]
    );
}

#[test]
fn driver_errors_are_bounded_before_rendering() {
    let error = (0..20)
        .map(|line| format!("provider diagnostic line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let compact = compact_driver_error(&error);

    assert_eq!(compact.lines().count(), 7);
    assert!(compact.ends_with('…'));
    assert!(!compact.contains("provider diagnostic line 6"));

    let long = compact_driver_error(&"x".repeat(2_000));
    assert_eq!(long.chars().count(), 800);
    assert!(long.ends_with('…'));
}

#[test]
fn session_navigation_tracks_back_forward_and_new_branches() {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let third = Uuid::new_v4();
    let branch = Uuid::new_v4();
    let mut navigation = SessionNavigation::default();

    navigation.visit(Some(first), second);
    navigation.visit(Some(second), third);
    assert_eq!(navigation.go_back(third), Some(second));
    assert_eq!(navigation.go_back(second), Some(first));
    assert_eq!(navigation.go_forward(first), Some(second));

    navigation.visit(Some(second), branch);
    assert_eq!(navigation.go_forward(branch), None);
    assert_eq!(navigation.go_back(branch), Some(second));
}

#[test]
fn session_navigation_prunes_deleted_tasks() {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let third = Uuid::new_v4();
    let mut navigation = SessionNavigation::default();

    navigation.visit(Some(first), second);
    navigation.visit(Some(second), third);
    assert_eq!(navigation.go_back(third), Some(second));

    navigation.remove(first);
    navigation.remove(third);
    assert_eq!(navigation.go_back(second), None);
    assert_eq!(navigation.go_forward(second), None);
}

#[test]
fn task_notification_tags_route_to_the_corresponding_task() {
    let session_id = Uuid::new_v4();
    let tag = task_notification_tag(session_id);

    assert_eq!(task_id_from_notification_tag(&tag), Some(session_id));
    assert_eq!(
        task_id_from_notification_tag("fintwind-task:not-a-uuid"),
        None
    );
    assert_eq!(task_id_from_notification_tag(&session_id.to_string()), None);
}

#[test]
fn conversation_navigation_rail_visibility_uses_all_three_gates() {
    assert!(should_show_navigation_rail(true, 2, 872.0));
    assert!(!should_show_navigation_rail(false, 2, 872.0));
    assert!(!should_show_navigation_rail(true, 1, 872.0));
    assert!(!should_show_navigation_rail(true, 2, 871.0));
}

#[test]
fn conversation_navigation_rail_height_caps_at_eighty_percent() {
    assert_eq!(navigation_rail_height(10, 600.0), 120.0);
    // Every turn keeps its full 12px scroll position; only the viewport is
    // capped, so the remaining turns are reached by scrolling the rail.
    assert_eq!(navigation_rail_height(100, 600.0), 480.0);
    assert!(navigation_rail_height(100, 600.0) <= 600.0 * 0.80);
    assert_eq!(
        NAVIGATION_RAIL_TURN_HEIGHT - NAVIGATION_RAIL_TICK_HEIGHT,
        10.0
    );
}

#[test]
fn conversation_navigation_rail_fades_point_toward_hidden_turns() {
    assert_eq!(
        navigation_rail_fade_visibility(px(0.0), px(120.0)),
        (false, true)
    );
    assert_eq!(
        navigation_rail_fade_visibility(px(-40.0), px(120.0)),
        (true, true)
    );
    assert_eq!(
        navigation_rail_fade_visibility(px(-120.0), px(120.0)),
        (true, false)
    );
    assert_eq!(
        navigation_rail_fade_visibility(px(0.0), px(0.0)),
        (false, false)
    );
}

#[test]
fn conversation_navigation_tick_scale_follows_hover_falloff() {
    assert_eq!(navigation_rail_scale(0, None), 0.25);
    assert_eq!(navigation_rail_scale(4, Some(4)), 1.0);
    assert_eq!(navigation_rail_scale(3, Some(4)), 0.68);
    assert_eq!(navigation_rail_scale(2, Some(4)), 0.44);
    assert_eq!(navigation_rail_scale(1, Some(4)), 0.25);
}

#[test]
fn conversation_navigation_active_turn_follows_the_scroll_top_and_tail() {
    let turn_rows = [0, 4, 9];
    assert_eq!(active_navigation_turn_index(&turn_rows, 0, false), Some(0));
    assert_eq!(active_navigation_turn_index(&turn_rows, 3, false), Some(0));
    assert_eq!(active_navigation_turn_index(&turn_rows, 4, false), Some(1));
    assert_eq!(active_navigation_turn_index(&turn_rows, 8, false), Some(1));
    assert_eq!(active_navigation_turn_index(&turn_rows, 4, true), Some(2));
    assert_eq!(active_navigation_turn_index(&[], 0, false), None);
}

#[test]
fn conversation_navigation_preview_uses_each_prompt_and_latest_response() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id);
    session.begin_turn("  First\n\nprompt  ");
    session.push_message(MessageRole::Assistant, "Interim update");
    session.push_message(MessageRole::Assistant, "Final answer");
    session.finish_active_turn(TurnStatus::Completed);
    session.begin_turn("Second prompt");
    let rows = [Message(0), Message(1), Message(2), Message(3)];

    let turns = transcript_navigation_turns(&session, &rows);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].message_index, 0);
    assert_eq!(turns[0].row_index, 0);
    assert_eq!(turns[0].prompt, "First prompt");
    assert_eq!(turns[0].response, "Final answer");
    assert_eq!(turns[1].row_index, 3);
    assert!(turns[1].response.is_empty());
    assert_eq!(
        navigation_preview_snippet("one   two\nthree", 20),
        "one two three"
    );
}

#[test]
fn conversation_navigation_preview_does_not_change_during_a_running_turn() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id);
    let session_id = session.id;
    session.begin_turn("Streaming prompt");
    append_text_delta_to_session(
        std::slice::from_mut(&mut session),
        session_id,
        false,
        "Partial".to_owned(),
    );
    let rows = [Message(0), Message(1)];
    let before = transcript_navigation_turns(&session, &rows);

    append_text_delta_to_session(
        std::slice::from_mut(&mut session),
        session_id,
        true,
        " response".to_owned(),
    );
    let during = transcript_navigation_turns(&session, &rows);

    assert_eq!(before, during);
    assert_eq!(during[0].response, "");

    session.finish_active_turn(TurnStatus::Completed);
    let completed = transcript_navigation_turns(&session, &rows);
    assert_eq!(completed[0].response, "Partial response");
}

#[test]
fn panel_widths_preserve_main_content_when_the_window_narrows() {
    let (sidebar, right_panel) = fitted_panel_widths(980.0, true, true, 420.0, 720.0);

    assert_eq!(sidebar, 340.0);
    assert_eq!(right_panel, 280.0);
    assert_eq!(980.0 - sidebar - right_panel, 360.0);
}

#[test]
fn hidden_panels_do_not_consume_layout_width() {
    let (sidebar, right_panel) = fitted_panel_widths(980.0, false, true, 420.0, 720.0);

    assert_eq!(sidebar, 0.0);
    assert_eq!(right_panel, 620.0);
}

#[test]
fn file_tree_width_preserves_a_usable_editor() {
    assert_eq!(fitted_file_tree_width(460.0, 184.0), 184.0);
    assert_eq!(fitted_file_tree_width(460.0, 400.0), 320.0);
    assert_eq!(fitted_file_tree_width(280.0, 184.0), 140.0);
    assert_eq!(fitted_file_tree_width(280.0, f32::NAN), 140.0);
}

#[test]
fn first_file_editor_opening_reserves_500_pixels() {
    assert_eq!(widened_panel_width_for_file_editor(460.0, 184.0), 684.0);
    assert_eq!(widened_panel_width_for_file_editor(720.0, 184.0), 720.0);
    assert_eq!(widened_panel_width_for_file_editor(460.0, 360.0), 860.0);
}

#[test]
fn first_review_opening_reserves_diff_and_tree_space() {
    assert_eq!(widened_panel_width_for_review(460.0), 820.0);
    assert_eq!(widened_panel_width_for_review(920.0), 920.0);
}

#[test]
fn anchor_end_space_keeps_a_short_new_turn_at_the_viewport_top() {
    assert_eq!(
        transcript_anchor_end_space(gpui::px(700.0), gpui::px(180.0)),
        gpui::px(520.0)
    );
    assert_eq!(
        transcript_anchor_end_space(gpui::px(700.0), gpui::px(900.0)),
        gpui::px(0.0)
    );
}

#[test]
fn scroll_to_bottom_only_appears_while_the_tail_is_below_the_viewport() {
    let viewport_bottom = px(700.0);

    assert_eq!(
        should_show_scroll_to_bottom(false, false, true, viewport_bottom, None, Pixels::ZERO),
        Some(false)
    );
    assert_eq!(
        should_show_scroll_to_bottom(
            true,
            true,
            true,
            viewport_bottom,
            Some(px(900.0)),
            Pixels::ZERO,
        ),
        Some(false)
    );
    // Disclosure pinning keeps `is_scrolled` true and a splice can leave the
    // tail temporarily unmeasured, but a collapsed transcript that fits the
    // viewport has nowhere to scroll back to.
    assert_eq!(
        should_show_scroll_to_bottom(true, false, false, viewport_bottom, None, Pixels::ZERO),
        Some(false)
    );
    assert_eq!(
        should_show_scroll_to_bottom(
            true,
            false,
            true,
            viewport_bottom,
            Some(px(701.0)),
            Pixels::ZERO,
        ),
        Some(true)
    );
    assert_eq!(
        should_show_scroll_to_bottom(
            true,
            false,
            true,
            viewport_bottom,
            Some(px(500.0)),
            px(200.0),
        ),
        Some(false)
    );
    // A stream commit remeasures the tail rows, so the frame after each one has
    // no bounds to read. Answering "show" there strobes the button against the
    // measured frames between commits; the caller holds its last answer instead.
    assert_eq!(
        should_show_scroll_to_bottom(true, false, true, viewport_bottom, None, Pixels::ZERO),
        None
    );
}

#[test]
fn scrolling_back_onto_the_tail_is_told_apart_from_an_unmeasured_tail() {
    let viewport_bottom = px(700.0);

    // Landed on the tail: the reply's last row ends at the viewport bottom,
    // with or without the anchor's reserved end space below it.
    assert_eq!(
        transcript_rests_at_tail(viewport_bottom, Some(px(700.0)), Pixels::ZERO),
        Some(true)
    );
    assert_eq!(
        transcript_rests_at_tail(viewport_bottom, Some(px(500.0)), px(200.0)),
        Some(true)
    );
    // Stopped short of it, so the stream must not reclaim the view.
    assert_eq!(
        transcript_rests_at_tail(viewport_bottom, Some(px(701.0)), Pixels::ZERO),
        Some(false)
    );
    // Unmeasured this frame: unknown, not "stopped short" — concluding the
    // latter would drop the re-engage whenever a scroll settles on a commit.
    assert_eq!(
        transcript_rests_at_tail(viewport_bottom, None, Pixels::ZERO),
        None
    );
}

#[test]
fn pending_expansion_reasserts_the_user_message_anchor() {
    let rows = gpui::ListState::new(3, gpui::ListAlignment::Bottom, gpui::px(0.0));
    rows.scroll_to(gpui::ListOffset {
        item_ix: 0,
        offset_in_item: gpui::px(42.0),
    });

    assert!(maintain_transcript_anchor(&rows, 0, true, gpui::px(320.0),));
    let anchored = rows.logical_scroll_top();
    assert_eq!(anchored.item_ix, 0);
    assert_eq!(anchored.offset_in_item, gpui::Pixels::ZERO);
    assert!(!maintain_transcript_anchor(
        &rows,
        0,
        true,
        gpui::Pixels::ZERO,
    ));
}

#[test]
fn settling_an_anchored_turn_splices_without_resetting_its_prompt() {
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("hi");
    session.push_message(MessageRole::Assistant, "Hello.");
    session.finish_active_turn(TurnStatus::Completed);

    let turn_id = session.begin_turn("give me a quick overview");
    session.status = SessionStatus::Working;
    session.transcript_blocks.push(TranscriptBlock {
        after_message: session.messages.len(),
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "Inspecting the project".into(),
                started_at_ms: 1_000,
                finished_at_ms: 2_000,
            },
            true,
        )],
    });
    session.push_message(MessageRole::Assistant, "Here is the overview.");

    let running = folded_transcript_row_kinds(&session, &HashSet::new());
    let anchor_row = running
        .iter()
        .position(|kind| *kind == Message(2))
        .expect("the second prompt is visible");
    let rows = ListState::new(running.len(), ListAlignment::Top, px(2048.0));
    rows.scroll_to(gpui::ListOffset {
        item_ix: anchor_row,
        offset_in_item: Pixels::ZERO,
    });

    session.status = SessionStatus::Idle;
    session.finish_active_turn(TurnStatus::Completed);
    let settled = folded_transcript_row_kinds(&session, &HashSet::new());
    let (range, new_count) = transcript_row_splice(&running, &settled)
        .expect("settlement folds the live work and removes its working row");

    assert!(
        range.start > anchor_row,
        "only rows after the anchored prompt should be folded"
    );
    rows.splice(range, new_count);
    assert_eq!(rows.item_count(), settled.len());
    let retained_anchor = rows.logical_scroll_top();
    assert_eq!(
        retained_anchor.item_ix, anchor_row,
        "an exact settlement splice must retain the sent-row anchor"
    );
    assert_eq!(retained_anchor.offset_in_item, Pixels::ZERO);
}

#[test]
fn only_later_user_messages_start_followup_turns() {
    let messages = vec![
        Message::new(MessageRole::User, "first"),
        Message::new(MessageRole::Assistant, "answer"),
        Message::new(MessageRole::User, "follow-up"),
        Message::new(MessageRole::Assistant, "answer"),
    ];
    assert!(!message_starts_followup_turn(&messages, 0));
    assert!(!message_starts_followup_turn(&messages, 1));
    assert!(message_starts_followup_turn(&messages, 2));
    assert!(!message_starts_followup_turn(&messages, 3));
}

#[test]
fn only_the_turn_opening_prompt_is_a_rewind_boundary() {
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("first prompt");
    session.push_message(MessageRole::Assistant, "working on it");
    // A steer the provider folded into the live turn.
    session.push_user_message_with_presentation("actually, also this", None, Vec::new());
    session.push_message(MessageRole::Assistant, "answer");
    session.finish_active_turn(TurnStatus::Interrupted);
    session.begin_turn("second prompt");

    assert!(message_opens_turn(&session.messages, 0));
    assert!(!message_opens_turn(&session.messages, 1));
    assert!(
        !message_opens_turn(&session.messages, 2),
        "a steer shares the turn's checkpoint, so it cannot be rewound to on its own"
    );
    assert!(!message_opens_turn(&session.messages, 3));
    assert!(message_opens_turn(&session.messages, 4));
}

#[test]
fn fenced_code_collects_all_blocks_without_languages() {
    let markdown = "Before\n```rust\nfn main() {}\n```\nAfter\n```\ncargo test\n```";
    assert_eq!(
        fenced_code(markdown).as_deref(),
        Some("fn main() {}\n\ncargo test")
    );
    assert_eq!(fenced_code("No code here"), None);
}

#[test]
fn stream_batches_commit_full_adjacent_text_and_preserve_event_order() {
    let runtime_id = Uuid::new_v4();
    let epoch = Uuid::new_v4();
    let mut events = VecDeque::from([
        DriverEvent::TextDelta {
            part: "text:msg_1:0".into(),
            delta: "first ".into(),
        },
        DriverEvent::RuntimeEventCursorAdvanced(RuntimeEventCursor {
            runtime_id,
            epoch,
            sequence: 1,
        }),
        DriverEvent::TextDelta {
            part: "text:msg_1:0".into(),
            delta: "line\nsecond line".into(),
        },
        DriverEvent::RuntimeEventCursorAdvanced(RuntimeEventCursor {
            runtime_id,
            epoch,
            sequence: 2,
        }),
        DriverEvent::Activity {
            id: None,
            kind: ActivityKind::Tool,
            title: "Tool".into(),
            detail: None,
            complete: true,
        },
        DriverEvent::TextDelta {
            part: "text:msg_1:0".into(),
            delta: "after tool".into(),
        },
    ]);

    assert!(matches!(
        pop_stream_batch(&mut events, StreamDeltaKind::Text),
        Some(DriverEvent::TextDelta { part, delta })
            if part == "text:msg_1:0" && delta == "first line\nsecond line"
    ));
    assert!(matches!(
        events.pop_front(),
        Some(DriverEvent::RuntimeEventCursorAdvanced(cursor)) if cursor.sequence == 2
    ));
    assert!(matches!(events.front(), Some(DriverEvent::Activity { .. })));
    assert!(matches!(
        events.get(1),
        Some(DriverEvent::TextDelta { part, delta })
            if part == "text:msg_1:0" && delta == "after tool"
    ));
}

#[test]
fn stream_parts_keep_targeting_the_running_session_after_selection_changes() {
    let project_id = uuid::Uuid::new_v4();
    let mut running = AgentSession::new(project_id);
    running.begin_turn("background task");
    running.status = SessionStatus::Working;
    let running_id = running.id;
    let visible = AgentSession::new(project_id);
    let visible_id = visible.id;
    let mut sessions = vec![running, visible];

    append_text_delta_to_session(&mut sessions, running_id, false, "first".into());
    // Navigation changes only which task is rendered. The runtime keeps
    // emitting with its own task ID while another task is visible.
    let selected_session = visible_id;
    append_text_delta_to_session(&mut sessions, running_id, true, " second".into());

    assert_eq!(selected_session, visible_id);
    assert_eq!(sessions[0].messages[1].content, "first second");
    assert!(sessions[0].messages[1].streaming);
    assert!(sessions[1].messages.is_empty());
}

#[test]
fn reasoning_and_tools_share_one_ordered_activity_block() {
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("Build it");

    push_transcript_activity(
        &mut session,
        ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "Inspecting the project".into(),
                started_at_ms: 1_000,
                finished_at_ms: 2_000,
            },
            true,
        ),
        false,
    );
    push_transcript_activity(
        &mut session,
        ActivityItem::new(None, ActivityKind::Command, "Ran tests", None, false),
        true,
    );

    assert_eq!(session.transcript_blocks.len(), 1);
    assert_eq!(session.transcript_blocks[0].activities.len(), 2);
    assert_eq!(
        session.transcript_blocks[0]
            .activities
            .iter()
            .map(|activity| activity.kind)
            .collect::<Vec<_>>(),
        [ActivityKind::Reasoning, ActivityKind::Command]
    );

    session.push_message(MessageRole::Assistant, "Interim update");
    push_transcript_activity(
        &mut session,
        ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "Checking the result".into(),
                started_at_ms: 3_000,
                finished_at_ms: 4_000,
            },
            false,
        ),
        false,
    );
    assert_eq!(
        session.transcript_blocks.len(),
        2,
        "assistant text keeps later work at its own transcript position"
    );
}

#[test]
fn completing_reasoning_identifies_the_row_that_must_be_remeasured() {
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("Build it");
    for content in ["first thought", "latest thought"] {
        session.transcript_blocks.push(TranscriptBlock {
            after_message: session.messages.len(),
            turn_id: session.active_turn_id(),
            activities: vec![ActivityItem::from_reasoning(
                ReasoningBlock {
                    content: content.into(),
                    started_at_ms: 1_000,
                    finished_at_ms: 2_000,
                },
                false,
            )],
        });
    }

    assert_eq!(
        complete_reasoning_activity_bound(&mut session, &[]),
        Some(1)
    );
    assert!(!session.transcript_blocks[0].activities[0].complete);
    assert!(session.transcript_blocks[1].activities[0].complete);
}

#[test]
fn a_reasoning_tail_delta_after_tool_events_returns_to_its_own_thought() {
    // Live opencode streams flush a thought's ~100ms-batched tail after the
    // next tool's events have already landed. Position alone would open a
    // stray one-character thought (the "!" bug); the fragment key routes the
    // tail back inside the thought it belongs to.
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("Find the web code");
    let mut open_reasoning = HashMap::new();
    let settled_reasoning = HashSet::new();
    let part = "reasoning:msg_1:0";

    bind_keyed_reasoning_delta(
        &mut session,
        &mut open_reasoning,
        &settled_reasoning,
        part,
        "**Searching** Let me look",
        true,
    );
    push_transcript_activity(
        &mut session,
        ActivityItem::new(
            Some("call_1".into()),
            ActivityKind::Search,
            "web",
            None,
            false,
        ),
        true,
    );
    let opened = bind_keyed_reasoning_delta(
        &mut session,
        &mut open_reasoning,
        &settled_reasoning,
        part,
        "!",
        true,
    );

    assert!(
        !opened,
        "a late tail delta must rejoin its fragment's activity"
    );
    assert_eq!(session.transcript_blocks.len(), 1, "no stray thought block");
    let activities = &session.transcript_blocks[0].activities;
    assert_eq!(activities.len(), 2);
    assert_eq!(
        activities[0].reasoning.as_ref().unwrap().content,
        "**Searching** Let me look!"
    );
    assert_eq!(activities[0].source_id.as_deref(), Some(part));

    // A different fragment key opens its own activity, still inside the same
    // continuing block — matching the stored part order a restart renders.
    let opened = bind_keyed_reasoning_delta(
        &mut session,
        &mut open_reasoning,
        &settled_reasoning,
        "reasoning:msg_2:0",
        "**Next**",
        true,
    );
    assert!(opened);
    assert_eq!(session.transcript_blocks.len(), 1);
    assert_eq!(session.transcript_blocks[0].activities.len(), 3);
}

#[test]
fn a_reasoning_fragment_settles_to_the_stored_transcript_shape() {
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("Find the web code");
    let mut open_reasoning = HashMap::new();
    let settled_reasoning = HashSet::new();
    let part = "reasoning:msg_1:0";

    bind_keyed_reasoning_delta(
        &mut session,
        &mut open_reasoning,
        &settled_reasoning,
        part,
        "partial",
        true,
    );
    // The durable end carries the authoritative full text, healing whatever
    // the delta stream lost or reordered.
    let settled = settle_keyed_reasoning_fragment(
        &mut session,
        &mut open_reasoning,
        part,
        Some("**Thought** full text!"),
        true,
    );
    assert_eq!(settled, Some(0));
    let activities = &session.transcript_blocks[0].activities;
    assert!(activities[0].complete);
    assert_eq!(
        activities[0].reasoning.as_ref().unwrap().content,
        "**Thought** full text!"
    );
    assert!(
        open_reasoning.is_empty(),
        "the fragment unbinds once settled"
    );

    // A fragment whose deltas never landed materializes from its end text.
    let settled = settle_keyed_reasoning_fragment(
        &mut session,
        &mut open_reasoning,
        "reasoning:msg_1:1",
        Some("late thought"),
        true,
    );
    assert_eq!(settled, None);
    let activities = &session.transcript_blocks[0].activities;
    assert_eq!(activities.len(), 2);
    assert!(activities[1].complete);
    assert_eq!(
        activities[1].reasoning.as_ref().unwrap().content,
        "late thought"
    );

    // A fragment that settles empty leaves nothing behind, like the replay
    // path's filter over stored parts.
    bind_keyed_reasoning_delta(
        &mut session,
        &mut open_reasoning,
        &settled_reasoning,
        "reasoning:msg_1:2",
        " ",
        true,
    );
    let settled = settle_keyed_reasoning_fragment(
        &mut session,
        &mut open_reasoning,
        "reasoning:msg_1:2",
        Some("  "),
        true,
    );
    // No block survives to remeasure — the empty activity (and its block,
    // had it emptied) is gone.
    assert_eq!(settled, None);
    assert_eq!(session.transcript_blocks[0].activities.len(), 2);
    assert!(open_reasoning.is_empty());
}

#[test]
fn a_tail_delta_after_its_fragment_settled_opens_nothing() {
    // The provider can flush a buffered tail after the fragment's own
    // authoritative end event. The settled key drops it: the end event already
    // wrote the full text, and the stored transcript holds no extra part.
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("Find the web code");
    let mut open_reasoning = HashMap::new();
    let mut settled_reasoning = HashSet::new();
    let part = "reasoning:msg_1:0";

    bind_keyed_reasoning_delta(
        &mut session,
        &mut open_reasoning,
        &settled_reasoning,
        part,
        "thought",
        true,
    );
    settle_keyed_reasoning_fragment(
        &mut session,
        &mut open_reasoning,
        part,
        Some("thought!"),
        true,
    );
    settled_reasoning.insert(part.to_owned());

    let opened = bind_keyed_reasoning_delta(
        &mut session,
        &mut open_reasoning,
        &settled_reasoning,
        part,
        "!",
        true,
    );
    assert!(
        !opened,
        "a settled fragment must not reopen for a stray tail"
    );
    assert_eq!(session.transcript_blocks.len(), 1);
    assert_eq!(session.transcript_blocks[0].activities.len(), 1);
    assert_eq!(
        session.transcript_blocks[0].activities[0]
            .reasoning
            .as_ref()
            .unwrap()
            .content,
        "thought!",
        "the authoritative end text is untouched"
    );
}

#[test]
fn stream_batches_keep_adjacent_text_parts_separate() {
    let mut events = VecDeque::from([
        DriverEvent::TextDelta {
            part: "text:msg_1:0".into(),
            delta: "了解".into(),
        },
        DriverEvent::TextDelta {
            part: "text:msg_1:0".into(),
            delta: "结构。".into(),
        },
        DriverEvent::TextDelta {
            part: "text:msg_1:1".into(),
            delta: "下一句".into(),
        },
    ]);

    assert!(matches!(
        pop_stream_batch(&mut events, StreamDeltaKind::Text),
        Some(DriverEvent::TextDelta { part, delta })
            if part == "text:msg_1:0" && delta == "了解结构。"
    ));
    assert!(matches!(
        pop_stream_batch(&mut events, StreamDeltaKind::Text),
        Some(DriverEvent::TextDelta { part, delta })
            if part == "text:msg_1:1" && delta == "下一句"
    ));
    assert!(events.is_empty());
}

#[test]
fn a_text_tail_after_a_tool_rejoins_its_part_and_a_new_part_starts_after() {
    // The provider flushes a text part's batched tail after the next tool's
    // events have already landed. The part key puts that tail back in the
    // sentence; a later ordinal is a real new part and stays after the tool.
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("look");
    let mut open_text = HashMap::new();
    let mut settled = HashSet::new();
    let part = "text:msg_1:0";

    assert!(open_keyed_text_part(
        &mut session,
        &mut open_text,
        &mut settled,
        part
    ));
    assert!(!bind_keyed_text_delta(
        &mut session,
        &mut open_text,
        &settled,
        part,
        "了解"
    ));
    push_transcript_activity(
        &mut session,
        ActivityItem::new(
            Some("call_1".into()),
            ActivityKind::FileRead,
            "read",
            None,
            false,
        ),
        false,
    );
    assert!(
        !bind_keyed_text_delta(&mut session, &mut open_text, &settled, part, "结构。"),
        "a late tail must rejoin the text part that started before the tool"
    );

    let assistant = session
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .collect::<Vec<_>>();
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0].content, "了解结构。");
    assert_eq!(session.transcript_blocks.len(), 1);
    assert_eq!(session.transcript_blocks[0].after_message, 2);

    let settled_index =
        settle_keyed_text_fragment(&mut session, &mut open_text, part, Some("了解结构。"));
    assert!(matches!(
        settled_index,
        super::KeyedTextSettle::Rewritten(1)
    ));
    assert!(!session.messages[1].streaming);
    assert!(open_text.is_empty());
    // The runtime records the key as settled; the free function only unbinds
    // it. A tail after that record must not open another message.
    settled.insert(part.to_owned());
    assert!(
        !open_keyed_text_part(&mut session, &mut open_text, &mut settled, part),
        "a started event after the part settled must not open another message"
    );
    assert!(!bind_keyed_text_delta(
        &mut session,
        &mut open_text,
        &settled,
        part,
        "!"
    ));
    assert_eq!(session.messages[1].content, "了解结构。");
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .count(),
        1
    );

    assert!(open_keyed_text_part(
        &mut session,
        &mut open_text,
        &mut settled,
        "text:msg_1:1"
    ));
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .count(),
        2
    );
    assert_eq!(session.transcript_blocks[0].after_message, 2);
}

#[test]
fn an_empty_text_part_is_removed_and_the_tool_anchor_moves_back() {
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("look");
    let mut open_text = HashMap::new();
    let mut settled = HashSet::new();
    assert!(open_keyed_text_part(
        &mut session,
        &mut open_text,
        &mut settled,
        "text:msg_1:0"
    ));
    push_transcript_activity(
        &mut session,
        ActivityItem::new(
            Some("call_1".into()),
            ActivityKind::Tool,
            "shell",
            None,
            true,
        ),
        false,
    );
    assert_eq!(session.transcript_blocks[0].after_message, 2);

    assert!(matches!(
        settle_keyed_text_fragment(&mut session, &mut open_text, "text:msg_1:0", Some("")),
        super::KeyedTextSettle::Removed
    ));
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .count(),
        0
    );
    assert_eq!(session.transcript_blocks[0].after_message, 1);
}

#[test]
fn stream_batches_keep_adjacent_reasoning_fragments_separate() {
    let mut events = VecDeque::from([
        DriverEvent::ReasoningDelta {
            part: "reasoning:msg_1:0".into(),
            delta: "a".into(),
        },
        DriverEvent::ReasoningDelta {
            part: "reasoning:msg_1:0".into(),
            delta: "b".into(),
        },
        DriverEvent::ReasoningDelta {
            part: "reasoning:msg_1:1".into(),
            delta: "c".into(),
        },
    ]);

    assert!(matches!(
        pop_stream_batch(&mut events, StreamDeltaKind::Reasoning),
        Some(DriverEvent::ReasoningDelta { part, delta })
            if part == "reasoning:msg_1:0" && delta == "ab"
    ));
    assert!(matches!(
        pop_stream_batch(&mut events, StreamDeltaKind::Reasoning),
        Some(DriverEvent::ReasoningDelta { part, delta })
            if part == "reasoning:msg_1:1" && delta == "c"
    ));
    assert!(events.is_empty());
}

#[test]
fn idle_reaping_releases_finished_sessions_but_never_a_running_turn() {
    let project_id = uuid::Uuid::new_v4();
    let fresh = Duration::from_secs(60);
    let stale = Duration::from_secs(60 * 60);

    let idle = AgentSession::new(project_id);
    assert!(session_is_reapable(Some(&idle), stale, false));
    assert!(!session_is_reapable(Some(&idle), fresh, false));
    assert!(!session_is_reapable(Some(&idle), stale, true));

    let mut working = AgentSession::new(project_id);
    working.begin_turn("a long tool call");
    working.status = SessionStatus::Working;
    assert!(!session_is_reapable(Some(&working), stale, false));

    // An approval can sit unanswered far longer than the idle window; its agent
    // is blocked on the user, not abandoned.
    let mut waiting = AgentSession::new(project_id);
    waiting.begin_turn("needs approval");
    waiting.status = SessionStatus::Waiting;
    assert!(!session_is_reapable(Some(&waiting), stale, false));

    let mut failed = AgentSession::new(project_id);
    failed.begin_turn("failed turn");
    failed.finish_active_turn(TurnStatus::Failed);
    failed.status = SessionStatus::Failed;
    assert!(session_is_reapable(Some(&failed), stale, false));

    // A runtime whose session is already gone is pure leak.
    assert!(session_is_reapable(None, stale, false));
}

#[test]
fn turn_blocks_keep_their_message_boundaries() {
    // user, assistant text, tool row, assistant text, reasoning row,
    // assistant text
    let rows = transcript_row_kinds(4, &[2, 3]);
    assert_eq!(
        rows,
        vec![
            Message(0),
            Message(1),
            TurnBlock(0),
            Message(2),
            TurnBlock(1),
            Message(3)
        ]
    );
}

#[test]
fn blocks_follow_the_latest_message_without_a_reply() {
    let rows = transcript_row_kinds(2, &[2]);
    assert_eq!(rows, vec![Message(0), Message(1), TurnBlock(0)]);
}

#[test]
fn plain_transcript_maps_one_to_one() {
    let rows = transcript_row_kinds(4, &[]);
    assert_eq!(rows, vec![Message(0), Message(1), Message(2), Message(3)]);
}

#[test]
fn multiple_blocks_at_one_boundary_preserve_event_order() {
    let rows = transcript_row_kinds(2, &[1, 1]);
    assert_eq!(
        rows,
        vec![Message(0), TurnBlock(0), TurnBlock(1), Message(1)]
    );
}

/// Row *kinds* and row *count* are derived from the same list, and
/// `transcript_row` looks a row up by index in the cached kinds. If the two
/// ever disagree — or the cache is left empty — every row silently falls back
/// to `Message(n)` and all reasoning and tool activity vanish from the
/// transcript. That is exactly the bug this guards.
/// Expanding a disclosure pins a short transcript to the bottom of its
/// viewport. Doing that needs the document's real height — treating an
/// unmeasured list as zero-height asks for a leading space of the entire
/// viewport, which pushes every row off screen and leaves the transcript blank
/// until the reader scrolls it back.
#[test]
fn a_disclosure_never_forces_a_scroll_it_cannot_measure() {
    // Unmeasured: no scroll at all.
    assert_eq!(disclosure_leading_space(px(718.0), None), None);

    // Short content sits at the bottom, with the remainder as leading space.
    assert_eq!(
        disclosure_leading_space(px(718.0), Some(px(200.0))),
        Some(px(518.0))
    );

    // Content taller than the viewport needs no leading space, and never a
    // negative one.
    assert_eq!(
        disclosure_leading_space(px(718.0), Some(px(5_000.0))),
        Some(Pixels::ZERO)
    );
}

/// Expanding a disclosure re-measures exactly one row by splicing it in place.
/// That splice went dead while the row-kind cache was empty, so this pins the
/// behaviour it depends on: replacing one row with one row must not disturb the
/// list's contents.
#[test]
fn splicing_one_row_in_place_preserves_the_list() {
    let list = ListState::new(6, ListAlignment::Bottom, px(2048.0));
    assert_eq!(list.item_count(), 6);

    list.splice(3..4, 1);
    assert_eq!(
        list.item_count(),
        6,
        "a 1-for-1 splice must keep the row count"
    );

    // The tail remeasure path splices a trailing window.
    list.splice(3..6, 3);
    assert_eq!(list.item_count(), 6);

    // A range at the very end is still valid.
    list.splice(5..6, 1);
    assert_eq!(list.item_count(), 6);
}

#[test]
fn row_kinds_and_row_count_describe_the_same_rows() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id);
    let turn_id = session.begin_turn("Build it");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 1,
        turn_id: Some(turn_id),
        activities: vec![
            ActivityItem::from_reasoning(
                ReasoningBlock {
                    content: "Looking around".into(),
                    started_at_ms: 1_000,
                    finished_at_ms: 2_000,
                },
                true,
            ),
            ActivityItem::new(None, ActivityKind::Command, "Ran tests", None, true),
        ],
    });
    session.push_message(MessageRole::Assistant, "Done.");
    session.finish_active_turn(TurnStatus::Completed);

    let kinds = folded_transcript_row_kinds(&session, &HashSet::from([turn_id]));
    // The work the agent did must be reachable by index, not just counted.
    assert!(
        kinds.iter().any(|kind| matches!(kind, TurnBlock(_))),
        "reasoning and activity must survive into the rendered rows: {kinds:?}"
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, TurnBlock(_)))
            .count(),
        1,
        "reasoning and tools share one activity cluster: {kinds:?}"
    );
    // Every index below the count resolves to a real row.
    for index in 0..kinds.len() {
        assert!(kinds.get(index).is_some());
    }
    // Collapsed, that same work is one fold row — reachable, not lost.
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), TurnFold(turn_id), Message(1)]
    );
}

#[test]
fn changed_files_attach_to_the_terminal_response_before_its_footer() {
    let mut session = AgentSession::new(Uuid::new_v4());
    let first_turn = session.begin_turn("Build it");
    session.push_message(MessageRole::Assistant, "Done.");
    session.finish_active_turn(TurnStatus::Completed);
    attach_changed_files(
        &mut session,
        vec![CheckpointFile {
            path: "src/app.rs".into(),
            additions: 12,
            deletions: 3,
        }],
    );

    session.begin_turn("One more thing");
    session.status = SessionStatus::Connecting;

    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), Message(1), Message(2), WorkingIndicator],
        "an inline card must not add a second transcript row before the next prompt"
    );
    assert_eq!(
        changed_files_inline_message_index(&session, first_turn),
        Some(1)
    );
}

#[test]
fn changed_files_remain_visible_when_an_interrupted_turn_has_no_answer() {
    let mut session = AgentSession::new(Uuid::new_v4());
    let turn_id = session.begin_turn("Make the change");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 1,
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::new(
            None,
            ActivityKind::Command,
            "Edited a file",
            None,
            true,
        )],
    });
    session.push_message(MessageRole::Assistant, "");
    session.finish_active_turn(TurnStatus::Interrupted);
    attach_changed_files(
        &mut session,
        vec![CheckpointFile {
            path: "src/lib.rs".into(),
            additions: 1,
            deletions: 0,
        }],
    );

    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), TurnFold(turn_id), ChangedFiles(turn_id)]
    );
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::from([turn_id])),
        vec![
            Message(0),
            TurnFold(turn_id),
            TurnBlock(0),
            Message(1),
            ChangedFiles(turn_id),
        ]
    );
    assert_eq!(changed_files_inline_message_index(&session, turn_id), None);
}

#[test]
fn changed_files_surface_appears_only_for_a_ready_nonempty_checkpoint() {
    let mut session = AgentSession::new(Uuid::new_v4());
    let turn_id = session.begin_turn("Build it");
    session.push_message(MessageRole::Assistant, "Done.");
    session.finish_active_turn(TurnStatus::Completed);

    attach_changed_files(&mut session, Vec::new());
    assert!(
        !folded_transcript_row_kinds(&session, &HashSet::new()).contains(&ChangedFiles(turn_id))
    );

    attach_changed_files(
        &mut session,
        vec![CheckpointFile {
            path: "src/main.rs".into(),
            additions: 2,
            deletions: 1,
        }],
    );
    assert_eq!(
        changed_files_inline_message_index(&session, turn_id),
        Some(1)
    );
    assert!(
        !folded_transcript_row_kinds(&session, &HashSet::new()).contains(&ChangedFiles(turn_id)),
        "a response with visible text hosts the card inside its terminal message"
    );
    session.turns[0]
        .checkpoint
        .as_mut()
        .expect("checkpoint")
        .status = CheckpointStatus::Unavailable;
    assert!(
        !folded_transcript_row_kinds(&session, &HashSet::new()).contains(&ChangedFiles(turn_id))
    );
    assert_eq!(changed_files_inline_message_index(&session, turn_id), None);
}

#[test]
fn checkpoint_completion_invalidates_the_cached_transcript_rows() {
    let mut session = AgentSession::new(Uuid::new_v4());
    let turn_id = session.begin_turn("Build it");
    session.push_message(MessageRole::Assistant, "Done.");
    session.finish_active_turn(TurnStatus::Completed);
    let before = transcript_rows_fingerprint(&session, &HashSet::new());

    attach_changed_files(
        &mut session,
        vec![CheckpointFile {
            path: "src/main.rs".into(),
            additions: 2,
            deletions: 1,
        }],
    );

    assert_ne!(
        transcript_rows_fingerprint(&session, &HashSet::new()),
        before
    );
    assert_eq!(
        changed_files_inline_message_index(&session, turn_id),
        Some(1)
    );
    assert!(
        !folded_transcript_row_kinds(&session, &HashSet::new()).contains(&ChangedFiles(turn_id)),
        "checkpoint completion changes the existing terminal message's height"
    );
}

#[test]
fn an_inline_checkpoint_keeps_followup_row_identity() {
    let previous = vec![Message(0), Message(1), Message(2), WorkingIndicator];
    let with_checkpoint = vec![Message(0), Message(1), Message(2), WorkingIndicator];

    assert_eq!(
        transcript_row_splice(&previous, &with_checkpoint),
        None,
        "the card remeasures its terminal message instead of shifting the following prompt"
    );
}

/// `refresh_transcript_row_kinds` skips the fold while this fingerprint holds
/// still, so anything that moves the rows has to move the fingerprint too.
/// Missing one leaves the transcript rendering stale rows, which drops every
/// reasoning block and tool activity from the session.
#[test]
fn the_row_fingerprint_moves_whenever_the_fold_does() {
    let mut base = AgentSession::new(Uuid::new_v4());
    let turn_id = base.begin_turn("Build it");
    base.transcript_blocks.push(TranscriptBlock {
        after_message: 1,
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "Looking around".into(),
                started_at_ms: 1_000,
                finished_at_ms: 2_000,
            },
            true,
        )],
    });
    base.push_message(MessageRole::Assistant, "I found the relevant code.");
    base.transcript_blocks.push(TranscriptBlock {
        after_message: 2,
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::new(
            None,
            ActivityKind::Command,
            "Ran tests",
            None,
            true,
        )],
    });
    base.push_message(MessageRole::Assistant, "Done. The change is ready.");
    base.finish_active_turn(TurnStatus::Completed);

    let settled = HashSet::new();
    // Both states, because a mutation inside the fold only moves rows once the
    // reader opens it — and those rows have to be right when they do.
    let rows = |session: &AgentSession| {
        (
            folded_transcript_row_kinds(session, &settled),
            folded_transcript_row_kinds(session, &HashSet::from([turn_id])),
        )
    };
    let baseline_rows = rows(&base);
    let baseline_fingerprint = transcript_rows_fingerprint(&base, &settled);

    // Expansion lives outside the session, so check it against the same base.
    assert_ne!(
        transcript_rows_fingerprint(&base, &HashSet::from([turn_id])),
        baseline_fingerprint,
        "expanding a turn fold"
    );

    let mutations: Vec<(&str, fn(&mut AgentSession))> = vec![
        ("a new message", |session| {
            session.push_message(MessageRole::User, "One more thing");
        }),
        ("a new transcript block", |session| {
            session.transcript_blocks.push(TranscriptBlock {
                after_message: session.messages.len(),
                turn_id: session.turns.first().map(|turn| turn.id),
                activities: vec![ActivityItem::new(
                    None,
                    ActivityKind::Command,
                    "Ran tests",
                    None,
                    true,
                )],
            });
        }),
        // `update_activity` re-anchors the block it is still appending to, so
        // the anchor cannot be treated as fixed at insertion.
        ("an existing block re-anchored", |session| {
            session.transcript_blocks[0].after_message = 2;
        }),
        ("a turn returning to running", |session| {
            session.turns[0].status = TurnStatus::Running;
        }),
        ("a message reassigned to no turn", |session| {
            session.messages[1].turn_id = None;
        }),
        // A blank part is work, not answer, so where the answer starts — and
        // with it everything the fold swallows — turns on the content itself.
        ("the final text part left blank", |session| {
            session.messages[2].content.clear();
        }),
    ];

    for (description, mutate) in mutations {
        let mut session = base.clone();
        mutate(&mut session);
        assert_ne!(
            rows(&session),
            baseline_rows,
            "{description} should change the rows — the case no longer proves anything"
        );
        assert_ne!(
            transcript_rows_fingerprint(&session, &settled),
            baseline_fingerprint,
            "{description} changed the rows but not the fingerprint, so the \
             cached rows would go stale"
        );
    }

    // The point of the guard: streamed text lands in an existing message
    // without moving a single row, and must not trigger a refold.
    let mut streamed = base.clone();
    streamed.messages[2].content.push_str(" Let me know.");
    assert_eq!(
        transcript_rows_fingerprint(&streamed, &settled),
        baseline_fingerprint,
        "appending to a message leaves the rows exactly where they were"
    );
}

#[test]
fn a_settled_turn_folds_all_of_its_work_above_the_answer() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id);
    let turn_id = session.begin_turn("Build it");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 1,
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "Looking around".into(),
                started_at_ms: 1_000,
                finished_at_ms: 2_000,
            },
            true,
        )],
    });
    session.push_message(MessageRole::Assistant, "I found the relevant code.");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 2,
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::new(
            None,
            ActivityKind::Command,
            "Ran tests",
            None,
            true,
        )],
    });
    session.push_message(MessageRole::Assistant, "Done. The change is ready.");
    session.finish_active_turn(TurnStatus::Completed);

    // The summary opens the turn and everything the agent did before its
    // answer sits behind it — reasoning, tool activity and interim commentary
    // alike. A divider between two pieces of work would read as a cut-off
    // response.
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), TurnFold(turn_id), Message(2)]
    );
    // Expanding restores the turn's real order in place.
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::from([turn_id])),
        vec![
            Message(0),
            TurnFold(turn_id),
            TurnBlock(0),
            Message(1),
            TurnBlock(1),
            Message(2)
        ]
    );
}

/// Providers split one answer across several text parts. They arrive with no
/// work between them, so they are all answer and none of them folds.
#[test]
fn consecutive_trailing_text_parts_all_stay_out_of_the_fold() {
    let mut session = AgentSession::new(Uuid::new_v4());
    let turn_id = session.begin_turn("Build it");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 1,
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "Looking around".into(),
                started_at_ms: 1_000,
                finished_at_ms: 2_000,
            },
            true,
        )],
    });
    session.push_message(MessageRole::Assistant, "First half of the answer.");
    session.push_message(MessageRole::Assistant, "Second half of the answer.");
    session.finish_active_turn(TurnStatus::Completed);

    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), TurnFold(turn_id), Message(1), Message(2)]
    );
}

/// An interrupted turn that never produced text has nothing to stay visible,
/// so the whole turn folds behind its summary rather than spilling raw work.
#[test]
fn a_turn_without_an_answer_folds_completely() {
    let mut session = AgentSession::new(Uuid::new_v4());
    let turn_id = session.begin_turn("Build it");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 1,
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::new(
            None,
            ActivityKind::Command,
            "Ran tests",
            None,
            true,
        )],
    });
    // The streaming placeholder never received any text.
    session.push_message(MessageRole::Assistant, "");
    session.finish_active_turn(TurnStatus::Interrupted);

    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), TurnFold(turn_id)]
    );
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::from([turn_id])),
        vec![Message(0), TurnFold(turn_id), TurnBlock(0), Message(1)]
    );
}

#[test]
fn assistant_response_footer_is_owned_by_the_terminal_part_and_copies_the_visible_answer() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id);
    let turn_id = session.begin_turn("Build it");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 1,
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "Looking around".into(),
                started_at_ms: 1_000,
                finished_at_ms: 2_000,
            },
            true,
        )],
    });
    session.push_message(MessageRole::Assistant, "Interim commentary.");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 2,
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::new(
            None,
            ActivityKind::Command,
            "Ran tests",
            None,
            true,
        )],
    });
    session.push_message(MessageRole::Assistant, "First half of the answer.");
    session.push_message(MessageRole::Assistant, "Second half of the answer.");
    session.finish_active_turn(TurnStatus::Completed);
    session.messages[3].created_at = 100;
    session.turns.last_mut().unwrap().completed_at = Some(200);

    assert_eq!(assistant_response_footer_index(&session, 1), Some(3));
    assert_eq!(assistant_response_footer_index(&session, 3), Some(3));
    assert_eq!(assistant_response_footer(&session, 1), None);
    // The interim commentary hides behind the "Worked for X" fold, so copying
    // the message must skip it and combine only the trailing answer parts.
    assert_eq!(
        assistant_response_footer(&session, 3).as_deref(),
        Some("First half of the answer.\n\nSecond half of the answer.")
    );
    assert_eq!(assistant_response_footer_time(&session, 1), None);
    assert_eq!(assistant_response_footer_time(&session, 3), Some(200));
    assert!(
        session.messages[1..]
            .iter()
            .all(|message| message.turn_id == Some(turn_id))
    );
}

/// A blank part breaks the answer run the same way work does — the fold hides
/// the text before it, so the copied message must leave that text out too.
#[test]
fn assistant_response_footer_treats_a_blank_part_as_work() {
    let mut session = AgentSession::new(Uuid::new_v4());
    let turn_id = session.begin_turn("Build it");
    session.push_message(MessageRole::Assistant, "First text part.");
    session.push_message(MessageRole::Assistant, "  ");
    session.push_message(MessageRole::Assistant, "Final text part.");
    session.finish_active_turn(TurnStatus::Completed);

    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), TurnFold(turn_id), Message(3)]
    );
    assert_eq!(
        assistant_response_footer(&session, 3).as_deref(),
        Some("Final text part.")
    );
}

#[test]
fn running_assistant_response_withholds_its_footer() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id);
    session.begin_turn("Keep going");
    session.push_message(MessageRole::Assistant, "Interim text.");

    assert_eq!(assistant_response_footer_index(&session, 1), None);
    assert_eq!(assistant_response_footer(&session, 1), None);
}

#[test]
fn unkeyed_assistant_message_keeps_a_standalone_footer() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id);
    session
        .messages
        .push(Message::new(MessageRole::Assistant, "Standalone response."));
    session.messages[0].created_at = 300;

    assert_eq!(assistant_response_footer_index(&session, 0), Some(0));
    assert_eq!(
        assistant_response_footer(&session, 0).as_deref(),
        Some("Standalone response.")
    );
    assert_eq!(assistant_response_footer_time(&session, 0), Some(300));
}

/// The TUI's `Locale.duration` ladder, reproduced band for band so a
/// Fintwind footer reads exactly like the TUI's for the same turn.
#[test]
fn turn_stats_duration_follows_the_tui_ladder() {
    assert_eq!(format_turn_stats_duration(0), "0ms");
    assert_eq!(format_turn_stats_duration(999), "999ms");
    assert_eq!(format_turn_stats_duration(1_000), "1.0s");
    assert_eq!(format_turn_stats_duration(25_940), "25.9s");
    // 59.999s rounds up inside its own band, matching toFixed(1).
    assert_eq!(format_turn_stats_duration(59_999), "60.0s");
    assert_eq!(format_turn_stats_duration(60_000), "1m 0s");
    assert_eq!(format_turn_stats_duration(3_599_999), "59m 59s");
    assert_eq!(format_turn_stats_duration(3_600_000), "1h 0m");
    assert_eq!(format_turn_stats_duration(86_399_999), "23h 59m");
    assert_eq!(format_turn_stats_duration(86_400_000), "1d 0h");
    assert_eq!(format_turn_stats_duration(180_000_000), "2d 2h");
}

/// Every segment of the stats line drops out when its input is missing, and
/// a line with nothing measurable to say does not render at all.
#[test]
fn turn_stats_line_assembles_segments_and_titlecases_the_agent() {
    // The TUI's own example line, rebuilt end to end.
    assert_eq!(
        turn_stats_line(
            Some("build"),
            Some("MiMo V2.5 Free"),
            Some(25_900),
            645,
            10_000
        ),
        Some("Build · MiMo V2.5 Free · 25.9s · 64.5 tok/s".to_owned())
    );
    // Titlecase capitalizes the head of every word, like the TUI's regex.
    assert_eq!(
        turn_stats_line(Some("code-review"), None, None, 0, 0),
        Some("Code-Review".to_owned())
    );
    // Unmeasurable throughput (no output, or no streaming time) drops the
    // segment rather than showing "0.0 tok/s".
    assert_eq!(
        turn_stats_line(Some("plan"), None, None, 0, 10_000).as_deref(),
        Some("Plan")
    );
    assert_eq!(
        turn_stats_line(None, None, Some(90_000), 10, 0).as_deref(),
        Some("1m 30s")
    );
    // No completion time drops only the duration segment. The model and
    // throughput segments can only come from captured turn statistics, so
    // this combination means a turn that reported tokens without a model.
    assert_eq!(
        turn_stats_line(None, Some("glmcoding/glm-5.3"), None, 645, 10_000).as_deref(),
        Some("glmcoding/glm-5.3 · 64.5 tok/s")
    );
    // A turn without statistics shows neither model nor throughput — the
    // session's current model is not what a historical turn ran — leaving at
    // most the agent and the duration (both assembled by the caller).
    assert_eq!(
        turn_stats_line(Some("plan"), None, Some(90_000), 0, 0).as_deref(),
        Some("Plan · 1m 30s")
    );
    assert_eq!(turn_stats_line(None, None, None, 0, 0), None);
}

#[test]
fn turn_tokens_per_second_requires_both_sides() {
    assert_eq!(turn_tokens_per_second(0, 5_000), None);
    assert_eq!(turn_tokens_per_second(645, 0), None);
    assert_eq!(turn_tokens_per_second(645, 10_000), Some(64.5));
}

/// The stats line is cached under the row-kinds fingerprint, so the stats'
/// own arrival has to move that fingerprint — a hydration or replay that
/// changes nothing else would leave the cached line missing otherwise.
#[test]
fn transcript_fingerprint_covers_turn_stats_appearance() {
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("Build it");
    session.push_message(MessageRole::Assistant, "Done.");
    session.finish_active_turn(TurnStatus::Completed);
    let without = transcript_rows_fingerprint(&session, &HashSet::new());
    session.turns.last_mut().unwrap().stats = Some(TurnStats {
        model: Some("glmcoding/glm-5.3".into()),
        agent: Some("build".into()),
        output_tokens: 645,
        stream_ms: 10_000,
    });
    let with = transcript_rows_fingerprint(&session, &HashSet::new());
    assert_ne!(without, with);
}

#[test]
fn completed_compaction_inserts_a_transcript_divider_once() {
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("Build it");
    session.push_message(MessageRole::Assistant, "Working.");
    let state = CompactionState {
        status: CompactionStatus::Completed,
        reason: Some("manual".into()),
        model: Some("rightcode/grok-4.6".into()),
        error: None,
        summary: Some("## Objective\n- Compacted.".into()),
    };
    upsert_compaction_transcript(&mut session, &state);
    upsert_compaction_transcript(&mut session, &state);

    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Compaction)
            .count(),
        1
    );
    let divider = session.messages.last().unwrap();
    assert_eq!(divider.role, MessageRole::Compaction);
    assert_eq!(divider.content, "## Objective\n- Compacted.");
    assert_eq!(divider.turn_id, None);
}

#[test]
fn turn_fold_visibility_splice_preserves_surrounding_message_rows() {
    let turn_id = Uuid::new_v4();
    let collapsed = vec![Message(0), TurnFold(turn_id), Message(2)];
    let expanded = vec![
        Message(0),
        TurnFold(turn_id),
        TurnBlock(0),
        Message(1),
        TurnBlock(1),
        Message(2),
    ];

    let expand_splice = transcript_row_splice(&collapsed, &expanded);
    assert_eq!(expand_splice, Some((2..2, 3)));
    assert_eq!(
        transcript_row_splice(&expanded, &collapsed),
        Some((2..5, 0))
    );
    assert_eq!(transcript_row_splice(&collapsed, &collapsed), None);
}

#[test]
fn running_turn_keeps_its_ordered_work_visible() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id);
    let turn_id = session.begin_turn("Keep going");
    session.transcript_blocks.push(TranscriptBlock {
        after_message: 1,
        turn_id: Some(turn_id),
        activities: vec![ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "Still thinking".into(),
                started_at_ms: 1_000,
                finished_at_ms: 2_000,
            },
            false,
        )],
    });
    session.push_message(MessageRole::Assistant, "Interim update");

    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), TurnBlock(0), Message(1)]
    );
}

#[test]
fn plain_settled_response_does_not_add_an_empty_work_fold() {
    let project_id = Uuid::new_v4();
    let mut session = AgentSession::new(project_id);
    session.begin_turn("Answer directly");
    session.push_message(MessageRole::Assistant, "The answer.");
    session.finish_active_turn(TurnStatus::Completed);

    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), Message(1)]
    );
}

#[test]
fn worked_duration_uses_readable_units() {
    assert_eq!(format_worked_duration(1), "1 second");
    assert_eq!(format_worked_duration(28), "28 seconds");
    assert_eq!(format_worked_duration(60), "1 minute");
    assert_eq!(format_worked_duration(88), "1 minute 28 seconds");
    assert_eq!(format_worked_duration(7_320), "2 hours 2 minutes");
}

#[test]
fn sidebar_time_labels_prefer_the_live_turn_over_the_last_reply() {
    use super::sidebar::{format_time_ago, session_time_label};

    assert_eq!(format_time_ago(0), "just now");
    assert_eq!(format_time_ago(59), "just now");
    assert_eq!(format_time_ago(300), "5m");
    assert_eq!(format_time_ago(7_200), "2h");
    assert_eq!(format_time_ago(420 * 86_400), "420d");

    // Never replied, nothing running: the row stays quiet.
    let mut session = AgentSession::new(Uuid::new_v4());
    assert_eq!(session_time_label(&session, 1_000), None);

    // A live turn counts up instead of showing the previous reply's age.
    session.last_reply_at = Some(40);
    session.begin_turn("go");
    session.status = SessionStatus::Working;
    session.turns[0].started_at = 100;
    assert_eq!(
        session_time_label(&session, 109).as_deref(),
        Some("Working for 9s")
    );

    // Settled again: back to how long ago the agent last replied.
    session.finish_active_turn(TurnStatus::Completed);
    session.status = SessionStatus::Idle;
    session.last_reply_at = Some(500);
    assert_eq!(session_time_label(&session, 800).as_deref(), Some("5m"));
}

/// The time-label wake-up chain arms exactly one timer, aimed at the next
/// instant a visible label rolls over. Firing early leaves a stale label on
/// screen for a unit; firing often burns wake-ups an idle window shouldn't
/// pay — so the boundary math is pinned here.
#[test]
fn time_label_wakes_land_exactly_on_label_boundaries() {
    use super::next_time_label_change;

    // Nothing on the clock: no sessions, or none that ever replied.
    assert_eq!(next_time_label_change(&[], 1_000), None);
    let mut session = AgentSession::new(Uuid::new_v4());
    assert_eq!(
        next_time_label_change(std::slice::from_ref(&session), 1_000),
        None
    );

    // "just now" becomes "1m" sixty seconds after the reply.
    session.last_reply_at = Some(1_000);
    let sessions = [session];
    assert_eq!(next_time_label_change(&sessions, 1_030), Some(30));
    // "1m" → "2m" at the next minute multiple, not on a fixed cadence.
    assert_eq!(next_time_label_change(&sessions, 1_090), Some(30));
    // Hours-old labels wake hourly…
    assert_eq!(
        next_time_label_change(&sessions, 1_000 + 3 * 3_600 + 1_200),
        Some(2_400)
    );
    // …and day-old labels daily.
    assert_eq!(
        next_time_label_change(&sessions, 1_000 + 2 * 86_400 + 3_600),
        Some(82_800)
    );

    // The earliest boundary across sessions wins.
    let mut fresher = AgentSession::new(Uuid::new_v4());
    fresher.last_reply_at = Some(1_000 + 2 * 86_400 + 3_550);
    let sessions = [&sessions[0], &fresher]
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        next_time_label_change(&sessions, 1_000 + 2 * 86_400 + 3_600),
        Some(10)
    );

    // A live turn pins the chain to seconds for its elapsed counter.
    let mut busy = AgentSession::new(Uuid::new_v4());
    busy.begin_turn("go");
    busy.status = SessionStatus::Working;
    let sessions = [busy];
    assert_eq!(next_time_label_change(&sessions, 1_030), Some(1));
}

#[test]
fn working_elapsed_stays_compact() {
    assert_eq!(format_working_elapsed(0), "0s");
    assert_eq!(format_working_elapsed(9), "9s");
    assert_eq!(format_working_elapsed(59), "59s");
    assert_eq!(format_working_elapsed(60), "1m");
    assert_eq!(format_working_elapsed(65), "1m 5s");
    assert_eq!(format_working_elapsed(3_600), "1h");
    assert_eq!(format_working_elapsed(3_720), "1h 2m");
}

/// The working indicator is on screen for the whole live turn: from before
/// the first chunk, below streamed content once chunks arrive, through the
/// permission pause — and gone the moment the session stops being busy.
#[test]
fn a_busy_turn_pins_the_working_indicator_after_the_last_row() {
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("Build it");
    session.status = SessionStatus::Working;

    // No chunks yet: the indicator alone follows the prompt.
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), WorkingIndicator]
    );

    // Streamed content pushes it down, never off.
    session.push_message(MessageRole::Assistant, "Starting on it.");
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), Message(1), WorkingIndicator]
    );

    // A pending permission keeps the turn — and the indicator — alive.
    session.status = SessionStatus::Waiting;
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), Message(1), WorkingIndicator]
    );

    // A driver error can fail the session while its last turn is still
    // marked running. Busy-ness is the only input that moved, so the
    // fingerprint must move with it or the stale indicator lingers.
    session.status = SessionStatus::Working;
    let busy_fingerprint = transcript_rows_fingerprint(&session, &HashSet::new());
    session.status = SessionStatus::Failed;
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), Message(1)]
    );
    assert_ne!(
        transcript_rows_fingerprint(&session, &HashSet::new()),
        busy_fingerprint,
        "dropping the busy status changed the rows but not the fingerprint"
    );

    // A settled turn swaps the indicator for its final transcript shape.
    session.status = SessionStatus::Working;
    session.finish_active_turn(TurnStatus::Completed);
    session.status = SessionStatus::Idle;
    assert_eq!(
        folded_transcript_row_kinds(&session, &HashSet::new()),
        vec![Message(0), Message(1)]
    );
}

#[test]
fn model_picker_highlight_wraps_at_both_ends() {
    // Nothing highlighted yet: down opens on the first row, up on the last.
    assert_eq!(next_picker_highlight(None, 3, "down"), Some(0));
    assert_eq!(next_picker_highlight(None, 3, "up"), Some(2));

    assert_eq!(next_picker_highlight(Some(0), 3, "down"), Some(1));
    assert_eq!(next_picker_highlight(Some(2), 3, "down"), Some(0));
    assert_eq!(next_picker_highlight(Some(0), 3, "up"), Some(2));

    // Keys the filter field owns must not move the cursor.
    assert_eq!(next_picker_highlight(Some(1), 3, "home"), None);
    assert_eq!(next_picker_highlight(Some(1), 3, "enter"), None);

    // An empty result list has nothing to land on.
    assert_eq!(next_picker_highlight(None, 0, "down"), None);
}

#[test]
fn settings_search_filters_pages_for_arrow_cycling() {
    use super::SettingsPage;

    let pages = |query: &str| {
        visible_settings_pages(query)
            .map(|(page, ..)| page)
            .collect::<Vec<_>>()
    };

    // An empty query keeps every page in sidebar order, so the arrows cycle
    // the full navigation even before anything is typed.
    let all_pages = vec![
        SettingsPage::General,
        SettingsPage::Appearance,
        SettingsPage::Providers,
        SettingsPage::Skills,
        SettingsPage::McpServers,
        SettingsPage::McpMarket,
        SettingsPage::Usage,
    ];
    assert_eq!(pages(""), all_pages);

    assert_eq!(pages("theme"), vec![SettingsPage::Appearance]);
    assert_eq!(pages("skill"), vec![SettingsPage::Skills]);
    assert_eq!(pages("heatmap"), vec![SettingsPage::Usage]);

    // A keyword shared across pages keeps them all reachable.
    assert_eq!(
        pages("codex"),
        vec![SettingsPage::Providers, SettingsPage::Skills]
    );

    assert_eq!(pages("no such setting"), vec![]);
}

#[test]
fn picker_filters_models_by_dynamic_provider_tabs() {
    use super::ModelPickerTab;
    use super::composer::{visible_picker_models, visible_picker_tabs};
    use crate::model::{FavoriteModel, ProviderModel, ProviderProbe};

    let probe = ProviderProbe {
        installed: true,
        path: Some(std::path::PathBuf::from("/bin/opencode")),
        models: vec![
            ProviderModel::new("openai/gpt-5", "GPT-5").sub_provider("OpenAI"),
            ProviderModel::new("anthropic/claude", "Claude").sub_provider("Anthropic"),
            ProviderModel::new("openai/gpt-5-mini", "GPT-5 Mini").sub_provider("OpenAI"),
            ProviderModel::new("local/model", "Local").sub_provider("opencode"),
            ProviderModel::new("fallback/model", "Fallback"),
        ],
        agent_presets: Vec::new(),
    };
    let probes = [probe];
    let favorites = [
        FavoriteModel {
            model: "anthropic/claude".into(),
        },
        FavoriteModel {
            model: "openai/gpt-5-mini".into(),
        },
    ];

    assert_eq!(
        visible_picker_tabs(&probes),
        vec![
            ModelPickerTab::Favorites,
            ModelPickerTab::Provider("OpenAI".into()),
            ModelPickerTab::Provider("Anthropic".into()),
            ModelPickerTab::Provider("OpenCode".into()),
        ]
    );

    let models = visible_picker_models(
        &probes,
        &favorites,
        ModelPickerTab::Provider("OpenAI".into()),
        "",
    );
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["openai/gpt-5", "openai/gpt-5-mini"]
    );

    let models = visible_picker_models(
        &probes,
        &favorites,
        ModelPickerTab::Provider("OpenCode".into()),
        "",
    );
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["local/model", "fallback/model"]
    );

    let models = visible_picker_models(&probes, &favorites, ModelPickerTab::Favorites, "");
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["anthropic/claude", "openai/gpt-5-mini"]
    );

    let models = visible_picker_models(
        &probes,
        &favorites,
        ModelPickerTab::Provider("Anthropic".into()),
        "openai",
    );
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["openai/gpt-5", "openai/gpt-5-mini"]
    );
}

#[test]
fn model_picker_provider_label_normalizes_the_opencode_fallback() {
    use super::composer::model_picker_provider_label;

    assert_eq!(model_picker_provider_label(Some("  ")), "OpenCode");
    assert_eq!(model_picker_provider_label(Some("OpenCode")), "OpenCode");
    assert_eq!(model_picker_provider_label(Some("OpenAI")), "OpenAI");
    assert_eq!(model_picker_provider_label(None), "OpenCode");
}

#[test]
fn tab_cycle_walks_favorites_then_dynamic_provider_rail() {
    use super::ModelPickerTab;
    use super::composer::visible_picker_tabs;
    use crate::model::{ProviderModel, ProviderProbe};

    let probes = [ProviderProbe {
        installed: true,
        path: Some(std::path::PathBuf::from("/bin/opencode")),
        models: vec![
            ProviderModel::new("openai/gpt", "GPT").sub_provider("OpenAI"),
            ProviderModel::new("anthropic/claude", "Claude").sub_provider("Anthropic"),
        ],
        agent_presets: Vec::new(),
    }];
    let tabs = visible_picker_tabs(&probes);
    assert_eq!(
        tabs,
        vec![
            ModelPickerTab::Favorites,
            ModelPickerTab::Provider("OpenAI".into()),
            ModelPickerTab::Provider("Anthropic".into()),
        ]
    );
    assert_eq!(next_picker_highlight(Some(2), tabs.len(), "down"), Some(0));
}

#[test]
fn provider_retry_card_previews_the_reason_and_clamps_it() {
    use super::transcript_view::compact_provider_retry_message;
    use crate::model::ProviderRetryAction;

    let action = ProviderRetryAction {
        reason: "free_tier_limit".into(),
        provider: "zen".into(),
        title: "Free limit reached".into(),
        message: "Subscribe to OpenCode Go".into(),
        label: "subscribe".into(),
        link: Some("https://opencode.ai/go".into()),
    };
    let retry = super::ProviderRetry {
        attempt: 2,
        message: "429 Too Many Requests".into(),
        action: Some(action),
        next_at_ms: Some(1_700_000_008_000),
        received_at_ms: 1_700_000_000_000,
    };
    // The header preview and the expanded body share this one-line clamp.
    assert_eq!(
        compact_provider_retry_message(&retry.message),
        "429 Too Many Requests"
    );

    let long = format!("{} tail", "x".repeat(80));
    let clipped = compact_provider_retry_message(&long);
    assert!(clipped.chars().count() <= 65);
    assert!(clipped.ends_with('…'));
    assert_eq!(
        compact_provider_retry_message("first line\nsecond line"),
        "first line"
    );
}

#[test]
fn provider_retry_stays_live_through_its_backoff_and_expires_after() {
    let received_at_ms = 1_700_000_000_000;
    let retry = super::ProviderRetry {
        attempt: 5,
        message: "Endpoint is unavailable.".into(),
        action: None,
        next_at_ms: Some(1_700_000_008_000),
        received_at_ms,
    };
    assert!(retry.is_live(1_700_000_000_000));
    assert!(retry.is_live(1_700_000_008_000));
    // The stamp prices the next attempt, not the report's expiry: an old
    // report may still describe a running backoff.
    assert!(retry.is_live(1_700_000_030_000));
    // Past the grace period nothing retracts the card but nothing renews it
    // either, so it stops claiming a retry.
    assert!(!retry.is_live(1_700_000_200_000));

    // opencode does not always stamp the next attempt. The report's own age
    // has to price the wait then — an unstamped report that never expires
    // would pin a card, and the pulse lease behind it, on screen forever.
    let unstamped = super::ProviderRetry {
        next_at_ms: None,
        ..retry
    };
    assert!(unstamped.is_live(received_at_ms));
    assert!(unstamped.is_live(received_at_ms + 59_000));
    assert!(!unstamped.is_live(received_at_ms + 61_000));
}

/// The retry card is app-level provider state, not turn output: it has to stay
/// on screen after the turn it belongs to has settled — precisely the window in
/// which the old working-row suffix disappeared and the reader saw nothing.
#[test]
fn provider_retry_card_outlives_the_turn_that_started_it() {
    let mut session = AgentSession::new(Uuid::new_v4());
    session.begin_turn("Build it");
    session.push_message(MessageRole::Assistant, "Starting on it.");
    session.finish_active_turn(TurnStatus::Completed);
    session.status = SessionStatus::Idle;
    let expanded = HashSet::new();

    let settled = folded_transcript_row_kinds(&session, &expanded);
    assert!(!settled.contains(&WorkingIndicator));
    assert!(!settled.contains(&ProviderRetry));

    let retrying = folded_transcript_row_kinds_with_retry(&session, &expanded, true);
    assert_eq!(retrying.last(), Some(&ProviderRetry));
    assert_ne!(
        transcript_rows_fingerprint_with_retry(&session, &expanded, true),
        transcript_rows_fingerprint(&session, &expanded),
        "the retry card changed the rows but not the fingerprint"
    );
}

#[test]
fn working_phase_labels_responding_and_only_long_silence() {
    use super::ProviderPhase;
    use super::transcript_view::{PROVIDER_QUIET_RESPONSE_SECS, WorkingPhase, working_phase};
    use crate::model::unix_time;

    let responding = ProviderPhase::Responding { since: 1_000 };
    let Some(WorkingPhase::Responding { elapsed_secs }) = working_phase(Some(&responding), 0)
    else {
        panic!("a busy report must label the row");
    };
    assert_eq!(elapsed_secs, unix_time() - 1_000);

    assert!(working_phase(None, PROVIDER_QUIET_RESPONSE_SECS - 1).is_none());
    assert!(matches!(
        working_phase(None, PROVIDER_QUIET_RESPONSE_SECS),
        Some(WorkingPhase::Awaiting { .. })
    ));
}
