"""Run the production transcript row planner with real session models, without UI."""

import contextlib
import ctypes
import importlib.util
import json
import os
import subprocess
import sys
import time
import uuid
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "target" / "goal-transcript-blackbox"
TRANSCRIPT = ROOT / "src" / "app" / "transcript.rs"


def extract_item(source: str, signature: str) -> str:
    start = source.index(signature)
    opening = source.index("{", start)
    depth = 0
    for index in range(opening, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    raise ValueError(f"unterminated Rust item: {signature}")


def main() -> None:
    source = TRANSCRIPT.read_text(encoding="utf-8")
    enum_start = source.index("#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]\npub(super) enum TranscriptRowKind")
    enum_end = source.index("#[derive(Clone, Debug, Eq, PartialEq)]", enum_start)
    row_enum = source[enum_start:enum_end].replace("pub(super)", "pub")
    functions = [
        extract_item(source, "pub(super) fn transcript_row_kinds("),
        extract_item(source, "pub(super) fn folded_transcript_row_kinds_with_retry("),
        extract_item(source, "pub(super) fn changed_files_inline_message_index("),
        extract_item(source, "pub(super) fn assistant_response_footer_index("),
        extract_item(source, "pub(super) fn assistant_response_footer("),
        extract_item(source, "pub(super) fn transcript_rows_fingerprint_with_retry("),
        extract_item(source, "pub(super) fn mix("),
        extract_item(source, "pub(super) fn mix_uuid("),
        extract_item(source, "fn mix_turn_id("),
        extract_item(source, "pub(super) fn goal_objective_message_indexes("),
        extract_item(source, "fn turn_rows("),
        extract_item(source, "fn turn_answer_start("),
        extract_item(source, "fn row_turn_id("),
    ]
    functions = [item.replace("pub(super)", "pub") for item in functions]

    TARGET.mkdir(parents=True, exist_ok=True)
    (TARGET / "Cargo.toml").write_text(
        """[package]
name = "goal-transcript-blackbox"
version = "0.0.0"
edition = "2024"

[workspace]

[dependencies]
fintwind-protocol = { path = "../../crates/fintwind-protocol" }
serde_json = "1.0"
base64 = "0.22"
uuid = { version = "1.18", features = ["v4", "serde"] }
""".replace('name = "goal-transcript-blackbox"', f'name = "{TARGET.name}"'),
        encoding="utf-8",
    )
    constants = "\n".join(
        line for line in source.splitlines()
        if line.startswith("pub(super) const EMPTY_TRANSCRIPT_FINGERPRINT")
        or line.startswith("const FINGERPRINT_PRIME")
    ).replace("pub(super)", "pub")
    rust = r'''use std::collections::{HashMap, HashSet};
use fintwind_protocol::{goal::{GoalSnapshot, GoalStatus}, model::{ActivityItem, ActivityKind, AgentSession, AgentTurn, Checkpoint, CheckpointFile, CheckpointStatus, CompactionStatus, Message, MessageRole, TranscriptBlock, TurnStatus}};
use uuid::Uuid;
mod fintwind_client { pub mod goal { pub use fintwind_protocol::goal::GoalStatus; } }

'''
    native_source = (ROOT / "crates/fintwind-core/src/driver/native.rs").read_text(encoding="utf-8")
    native_functions = [extract_item(native_source, signature) for signature in (
        "fn translate_native_rows(", "fn native_goal_objective(", "pub(crate) fn is_native_goal_kickoff(",
        "fn ms_to_seconds(", "fn native_image_attachment(",
        "fn turn_stats_step(", "fn assistant_parts_visible(", "fn append_assistant_parts(",
        "fn flush_assistant_text(", "fn flush_assistant_activities(", "fn parts_text(",
        "fn reasoning_item(", "fn tool_item(",
    )]
    native_functions[0] = native_functions[0].replace("fn translate_native_rows(", "pub fn translate_native_rows(", 1)
    rust += "\nmacro_rules! tr { ($key:expr) => { fintwind_protocol::i18n::translate($key) }; }\nmod model { pub use fintwind_protocol::model::*; }\n"
    for module, path in (("blob_store", "crates/fintwind-core/src/blob_store.rs"),
                         ("activity", "crates/fintwind-core/src/driver/activity.rs")):
        rust += '#[path = ' + json.dumps((ROOT / path).as_posix()) + '] mod ' + module + ';\n'
    rust += "\nmod native {\nuse std::collections::HashSet;\nuse base64::Engine as _;\nuse serde_json::Value;\n"
    rust += "use crate::blob_store::BlobStore;\nuse crate::model::{ActivityItem, AgentTurn, Message, MessageAttachment, MessageRole, ReasoningBlock, TranscriptBlock, TurnStats, TurnStatus};\n"
    rust += "use fintwind_protocol::provider_session::NativeTranscript;\nconst MAX_TRANSCRIPT_IMAGE_BYTES: usize = 128 * 1024 * 1024;\n"
    rust += "\n\n".join(native_functions) + "\n}\n"
    rust += constants + "\n\n" + row_enum + "\n\n" + "\n\n".join(functions)
    rust += r'''

fn turn(id: Uuid, started_at: u64, completed_at: Option<u64>, status: TurnStatus) -> AgentTurn {
    AgentTurn {
        id,
        submission: None,
        turn_count: 1,
        status,
        provider_turn_started: true,
        provider_resume_at: None,
        provider_prompt: None,
        goal_objective: false,
        started_at,
        completed_at,
        checkpoint: None,
        stats: None,
    }
}

fn main() {
    let old = Uuid::from_u128(10);
    let current = Uuid::from_u128(11);
    let future = Uuid::from_u128(12);
    let mut session = AgentSession::new(Uuid::from_u128(1));
    session.turns = vec![
        turn(old, 10, Some(20), TurnStatus::Completed),
        turn(current, 110, Some(201), TurnStatus::Completed),
        turn(future, 300, None, TurnStatus::Running),
    ];
    session.messages = vec![
        Message::new_for_turn(MessageRole::User, "old prompt", old),
        Message::new_for_turn(MessageRole::Assistant, "old response", old),
        Message::new_for_turn(MessageRole::User, "goal prompt", current),
        Message::new_for_turn(MessageRole::Assistant, "goal result", current),
        Message::new_for_turn(MessageRole::User, "later unrelated prompt", future),
    ];
    session.goal = Some(GoalSnapshot {
        session_id: session.id.to_string(), objective: "complete goal".into(),
        status: GoalStatus::Done, turns: 2, max_turns: 8, reason: "completed".into(),
        revision: 3, result: Some("done".into()), model: None, tools: None,
        started_at: Some(100), completed_at: Some(200), elapsed_seconds: None, active_since: None,
    });
    session = serde_json::from_value(serde_json::to_value(session).unwrap()).unwrap();
    let rows = folded_transcript_row_kinds_with_retry(&session, &HashSet::new(), false);
    assert_eq!(rows, vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::Message(1),
        TranscriptRowKind::Message(2), TranscriptRowKind::Message(3), TranscriptRowKind::Message(4),
    ], "completed goals do not add a separate transcript card");
    println!("settled: {:?}", rows);

    let tool_turn = Uuid::from_u128(13);
    let mut tool_session = AgentSession::new(Uuid::from_u128(2));
    tool_session.turns = vec![turn(tool_turn, 400, Some(410), TurnStatus::Completed)];
    tool_session.messages = vec![
        Message::new_for_turn(MessageRole::User, "tool prompt", tool_turn),
        Message::new_for_turn(MessageRole::Assistant, "tool response", tool_turn),
    ];
    tool_session.transcript_blocks = vec![TranscriptBlock {
        after_message: 1, turn_id: Some(tool_turn),
        activities: vec![ActivityItem::new(None, ActivityKind::Command, "Run", None, true)],
    }];
    let tool_rows = folded_transcript_row_kinds_with_retry(&tool_session, &HashSet::new(), false);
    assert_eq!(tool_rows, vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(tool_turn),
        TranscriptRowKind::Message(1),
    ], "tool activity folds beneath its turn summary");
    let mut no_answer = tool_session.clone();
    no_answer.messages.truncate(1);
    let no_answer_rows = folded_transcript_row_kinds_with_retry(&no_answer, &HashSet::new(), false);
    assert_eq!(no_answer_rows, vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(tool_turn),
    ], "turns without visible answers still produce a fold row");

    session.turns[1].status = TurnStatus::Running;
    session.turns[1].completed_at = None;
    session.turns.truncate(2);
    session.messages.truncate(4);
    session.status = fintwind_protocol::model::SessionStatus::Working;
    let rows = folded_transcript_row_kinds_with_retry(&session, &HashSet::new(), false);
    assert_eq!(rows, vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::Message(1),
        TranscriptRowKind::Message(2), TranscriptRowKind::Message(3),
        TranscriptRowKind::WorkingIndicator,
    ], "a running turn keeps its working indicator");
    println!("running: {:?}", rows);

    let mut guided = AgentSession::new(Uuid::from_u128(3));
    let guided_turn = guided.begin_turn("Build the feature.");
    guided.status = fintwind_protocol::model::SessionStatus::Working;
    guided.push_message(MessageRole::Assistant, "I will use the old approach.");
    guided.push_user_message_with_presentation("Use the new approach.", None, vec![]);
    guided.push_user_message_with_presentation("Keep the public API.", None, vec![]);
    guided.push_message(MessageRole::Assistant, "First half of the final answer.");
    guided.push_message(MessageRole::Assistant, "Second half of the final answer.");
    let live = folded_transcript_row_kinds_with_retry(&guided, &HashSet::new(), false);
    assert_eq!(live, vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::Message(1),
        TranscriptRowKind::Message(2), TranscriptRowKind::Message(3),
        TranscriptRowKind::Message(4), TranscriptRowKind::Message(5),
        TranscriptRowKind::WorkingIndicator,
    ], "guidance remains visible while the turn runs");
    guided.finish_active_turn(TurnStatus::Completed);
    guided.status = fintwind_protocol::model::SessionStatus::Idle;
    guided = serde_json::from_value(serde_json::to_value(guided).unwrap()).unwrap();
    let settled = folded_transcript_row_kinds_with_retry(&guided, &HashSet::new(), false);
    assert_eq!(settled, vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(guided_turn),
        TranscriptRowKind::Message(4), TranscriptRowKind::Message(5),
    ], "a completed turn folds both guidance messages and commentary, even without tools");
    let expanded = folded_transcript_row_kinds_with_retry(&guided, &HashSet::from([guided_turn]), false);
    assert_eq!(expanded, vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(guided_turn),
        TranscriptRowKind::Message(1), TranscriptRowKind::Message(2),
        TranscriptRowKind::Message(3), TranscriptRowKind::Message(4), TranscriptRowKind::Message(5),
    ], "expanding restores the complete original order");
    assert_eq!(assistant_response_footer(&guided, 5).as_deref(),
               Some("First half of the final answer.\n\nSecond half of the final answer."),
               "copying the answer excludes steering and earlier commentary");

    guided.transcript_blocks.push(TranscriptBlock {
        after_message: 3, turn_id: Some(guided_turn),
        activities: vec![ActivityItem::new(None, ActivityKind::Command, "Apply guidance", None, true)],
    });
    assert_eq!(folded_transcript_row_kinds_with_retry(&guided, &HashSet::new(), false), settled,
               "tool work between guidance messages belongs in the same fold");
    assert_eq!(folded_transcript_row_kinds_with_retry(&guided, &HashSet::from([guided_turn]), false), vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(guided_turn),
        TranscriptRowKind::Message(1), TranscriptRowKind::Message(2), TranscriptRowKind::TurnBlock(0),
        TranscriptRowKind::Message(3), TranscriptRowKind::Message(4), TranscriptRowKind::Message(5),
    ], "expansion interleaves guidance and tools at their original anchors");
    let next_turn = guided.begin_turn("An unrelated follow-up.");
    guided.status = fintwind_protocol::model::SessionStatus::Working;
    assert_ne!(next_turn, guided_turn);
    let next_rows = folded_transcript_row_kinds_with_retry(&guided, &HashSet::new(), false);
    assert_eq!(next_rows, vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(guided_turn),
        TranscriptRowKind::Message(4), TranscriptRowKind::Message(5),
        TranscriptRowKind::Message(6), TranscriptRowKind::WorkingIndicator,
    ], "folding the earlier turn never hides the next turn's opening prompt");

    let mut interrupted = AgentSession::new(Uuid::from_u128(4));
    let interrupted_turn = interrupted.begin_turn("Investigate.");
    interrupted.push_message(MessageRole::Assistant, "Initial approach.");
    interrupted.push_user_message_with_presentation("Change direction before proceeding.", None, vec![]);
    interrupted.finish_active_turn(TurnStatus::Interrupted);
    interrupted.status = fintwind_protocol::model::SessionStatus::Idle;
    assert_eq!(folded_transcript_row_kinds_with_retry(&interrupted, &HashSet::new(), false), vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(interrupted_turn),
    ], "guidance without a subsequent answer folds with the interrupted work");
    assert_eq!(folded_transcript_row_kinds_with_retry(&interrupted, &HashSet::from([interrupted_turn]), false), vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(interrupted_turn),
        TranscriptRowKind::Message(1), TranscriptRowKind::Message(2),
    ]);
    interrupted.turns[0].checkpoint = Some(Checkpoint {
        turn_count: 1, git_ref: "fixture".into(), status: CheckpointStatus::Ready,
        files: vec![CheckpointFile { path: "src/main.rs".into(), additions: 1, deletions: 0 }],
        additions: 1, deletions: 0, created_at: 1,
    });
    assert_eq!(folded_transcript_row_kinds_with_retry(&interrupted, &HashSet::new(), false), vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(interrupted_turn),
        TranscriptRowKind::ChangedFiles(interrupted_turn),
    ], "file changes remain visible when guidance arrived after the last assistant text");
    assert_eq!(goal_objective_message_indexes(&guided), HashSet::new(), "normal prompts and steering are not Goals");
    let mut marked = AgentSession::new(Uuid::from_u128(5));
    marked.begin_turn("Ship the Goal.");
    marked.mark_active_turn_goal();
    marked.push_user_message_with_presentation("Steering within the Goal.", None, vec![]);
    marked.push_message(MessageRole::Assistant, "Goal complete.");
    marked.finish_active_turn(TurnStatus::Completed);
    marked.begin_turn("An ordinary follow-up with the same text: Ship the Goal.");
    marked.finish_active_turn(TurnStatus::Completed);
    marked.begin_turn("A replacement Goal.");
    marked.mark_active_turn_goal();
    let marked: AgentSession = serde_json::from_value(serde_json::to_value(marked).unwrap()).unwrap();
    assert_eq!(goal_objective_message_indexes(&marked), HashSet::from([0, 4]),
               "Goal identity survives persistence and replacement without marking steering or normal prompts");
    let mut legacy = serde_json::to_value(&marked).unwrap();
    for saved_turn in legacy["turns"].as_array_mut().unwrap() {
        saved_turn.as_object_mut().unwrap().remove("goal_objective");
    }
    let legacy: AgentSession = serde_json::from_value(legacy).unwrap();
    assert_eq!(goal_objective_message_indexes(&legacy), HashSet::new(), "legacy unmarked turns load safely");
    let native_rows = serde_json::json!([
        {"type":"user","id":"msg_start","text":"Original request","time":{"created":1000}},
        {"type":"assistant","id":"msg_work1","time":{"created":2000,"completed":2500},"content":[
            {"type":"tool","id":"call_1","name":"read","state":{"status":"completed","input":{},"content":[{"type":"text","text":"source"}]}},
            {"type":"text","text":"Progress before guidance"}
        ]},
        {"type":"user","id":"msg_steer1","text":"Find the actual desktop style","time":{"created":3000}},
        {"type":"assistant","id":"msg_work2","time":{"created":4000,"completed":4500},"content":[
            {"type":"tool","id":"call_2","name":"read","state":{"status":"completed","input":{},"content":[{"type":"text","text":"reference"}]}}
        ]},
        {"type":"user","id":"msg_steer2","text":"Do not invent it","time":{"created":5000}},
        {"type":"assistant","id":"msg_final","time":{"created":6000,"completed":7000},"content":[{"type":"text","text":"Final answer"}]},
        {"type":"idle","id":"msg_idle1","outcome":"succeeded","time":{"created":8000}},
        {"type":"user","id":"msg_next","text":"A separate request","time":{"created":9000}},
        {"type":"assistant","id":"msg_next_answer","time":{"created":10000,"completed":10500},"content":[{"type":"text","text":"Separate answer"}]},
        {"type":"idle","id":"msg_idle2","outcome":"succeeded","time":{"created":11000}}
    ]);
    let imported = native::translate_native_rows(native_rows.as_array().unwrap(), None, false, None);
    assert_eq!(imported.turns.len(), 2, "native idle boundaries, not each user prompt, define complete turns");
    let imported_turn = imported.turns[0].id;
    let mut restored = AgentSession::new(Uuid::from_u128(6));
    restored.messages = imported.messages;
    restored.transcript_blocks = imported.blocks;
    restored.turns = imported.turns;
    restored = serde_json::from_value(serde_json::to_value(restored).unwrap()).unwrap();
    assert_eq!(folded_transcript_row_kinds_with_retry(&restored, &HashSet::new(), false), vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(imported_turn),
        TranscriptRowKind::Message(4), TranscriptRowKind::Message(5), TranscriptRowKind::Message(6),
    ], "history import and reload keep both steering messages inside one whole-turn fold");
    assert_eq!(folded_transcript_row_kinds_with_retry(&restored, &HashSet::from([imported_turn]), false), vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::TurnFold(imported_turn),
        TranscriptRowKind::TurnBlock(0), TranscriptRowKind::Message(1), TranscriptRowKind::Message(2),
        TranscriptRowKind::TurnBlock(1), TranscriptRowKind::Message(3), TranscriptRowKind::Message(4),
        TranscriptRowKind::Message(5), TranscriptRowKind::Message(6),
    ], "expanding the imported fold restores guidance and activities together in exact order");
    assert_eq!(assistant_response_footer(&restored, 4).as_deref(), Some("Final answer"));
    let mut local = restored.clone();
    local.turns[0].submission = Some(fintwind_protocol::submission::SubmissionReceipt {
        id: imported_turn, input_id: "msg_start".into(),
        state: fintwind_protocol::submission::SubmissionState::Accepted,
        created_at: 1000, updated_at: 1000, native_session_id: None,
    });
    local.turns[0].provider_prompt = Some("Original request".into());
    local.messages[0].content = "/original-command".into();
    local.messages[2].display_content = Some("Steering presentation".into());
    let merged = fintwind_protocol::submission::merge_native_transcript(&local,
        native::translate_native_rows(native_rows.as_array().unwrap(), None, false, None));
    assert_eq!(merged.messages.iter().filter(|m| m.role == MessageRole::User).map(|m| m.content.as_str()).collect::<Vec<_>>(),
        vec!["/original-command", "Find the actual desktop style", "Do not invent it", "A separate request"],
        "local merge must restore each steering message once instead of cloning the opener onto all users");
    assert_eq!(merged.messages[2].display_content.as_deref(), Some("Steering presentation"));
    local.messages.remove(3);
    let merged = fintwind_protocol::submission::merge_native_transcript(&local,
        native::translate_native_rows(native_rows.as_array().unwrap(), None, false, None));
    assert_eq!(merged.messages[3].content, "Do not invent it", "new native steering survives a stale local snapshot");
    let rows = native_rows.as_array().unwrap();
    let unfinished = native::translate_native_rows(&rows[..9], None, false, None);
    assert_eq!(unfinished.turns.len(), 2, "a missing final idle marker does not split an unfinished turn");
    let recovery = native::translate_native_rows(&rows[..6], None, true, None);
    assert_eq!(recovery.turns.len(), 1, "recovery retains all steering in the admitted execution window");
    let legacy = native::translate_native_rows(&rows[..6], None, false, None);
    assert_eq!(legacy.turns.len(), 3, "legacy histories without idle markers keep their original fallback");
    let mut provider_only = rows[..7].to_vec();
    provider_only.push(serde_json::json!({"type":"assistant", "time":{"created":9000,"completed":10000},
        "content":[{"type":"text","text":"Synthetic continuation"}]}));
    provider_only.push(serde_json::json!({"type":"idle","outcome":"interrupted","time":{"created":11000}}));
    let continued = native::translate_native_rows(&provider_only, None, false, None);
    assert_eq!(continued.turns.len(), 2, "provider-only execution after idle gets a separate owner");
    assert_eq!(continued.turns[1].status, TurnStatus::Interrupted);
    assert_eq!(continued.turns[0].completed_at, Some(8));
    assert_eq!(continued.turns[1].completed_at, Some(11));
    println!("raw native history -> import -> persisted reload -> whole-turn fold verified");
    println!("guided turn: collapsed={:?}, expanded={:?}; interrupted guidance verified", settled, expanded);
}
'''
    (TARGET / "src").mkdir(exist_ok=True)
    (TARGET / "src" / "main.rs").write_text(rust, encoding="utf-8")
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(ROOT / "target")
    result = subprocess.run(
        ["cargo", "run", "--offline", "--manifest-path", str(TARGET / "Cargo.toml")],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        print(result.stdout, end="")
        print(result.stderr, end="")
        result.check_returncode()
    print(result.stdout, end="")
    (TARGET / "report.json").write_text(
        json.dumps({"status": "passed", "output": result.stdout}, indent=2),
        encoding="utf-8",
    )


def run_isolated():
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("owned_process", ROOT / "scripts/goal-opencode-e2e.py")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    data = ROOT / "target" / ("goal-transcript-blackbox-" + uuid.uuid4().hex)
    data.mkdir(parents=True)
    allowed = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "CARGO_HOME", "RUSTUP_HOME",
               "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS", "INCLUDE", "LIB", "LIBPATH"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
    for key in ("HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP",
                "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
        directory = data / key.lower()
        directory.mkdir()
        env[key] = str(directory)
    child = helper.OwnedProcess(sys.executable, [str(Path(__file__).resolve()), "--worker", str(data)], data, env)
    try:
        deadline = time.monotonic() + 600
        while child.alive():
            if time.monotonic() > deadline:
                raise TimeoutError(f"Transcript black-box check exceeded ten minutes: {data}")
            time.sleep(0.1)
        status = ctypes.c_ulong()
        child.kernel.GetExitCodeProcess.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
        if not child.kernel.GetExitCodeProcess(child.process, ctypes.byref(status)):
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        child.close()
    if status.value != 0 or not (data / "report.json").is_file():
        raise RuntimeError((data / "worker.log").read_text(encoding="utf-8", errors="replace")[-7000:])
    print(f"[OK] Transcript folding black-box check: {data / 'report.json'}")


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--worker":
        TARGET = Path(sys.argv[2])
        with (TARGET / "worker.log").open("w", encoding="utf-8") as log:
            with contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
                main()
    else:
        run_isolated()
