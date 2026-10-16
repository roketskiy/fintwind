"""Run the production transcript row planner with real session models, without UI."""

import json
import os
import subprocess
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
        extract_item(source, "pub(super) fn transcript_rows_fingerprint_with_retry("),
        extract_item(source, "pub(super) fn mix("),
        extract_item(source, "pub(super) fn mix_uuid("),
        extract_item(source, "fn mix_turn_id("),
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
uuid = { version = "1.18", features = ["v4", "serde"] }
""",
        encoding="utf-8",
    )
    constants = "\n".join(
        line for line in source.splitlines()
        if line.startswith("pub(super) const EMPTY_TRANSCRIPT_FINGERPRINT")
        or line.startswith("const FINGERPRINT_PRIME")
    ).replace("pub(super)", "pub")
    rust = r'''use std::collections::{HashMap, HashSet};
use fintwind_protocol::{goal::{GoalSnapshot, GoalStatus}, model::{ActivityItem, ActivityKind, AgentSession, AgentTurn, CheckpointStatus, CompactionStatus, Message, MessageRole, TranscriptBlock, TurnStatus}};
use uuid::Uuid;
mod fintwind_client { pub mod goal { pub use fintwind_protocol::goal::GoalStatus; } }

'''
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
        started_at: Some(100), completed_at: Some(200),
    });
    session = serde_json::from_value(serde_json::to_value(session).unwrap()).unwrap();
    let rows = folded_transcript_row_kinds_with_retry(&session, &HashSet::new(), false);
    assert_eq!(rows, vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::Message(1),
        TranscriptRowKind::Message(2), TranscriptRowKind::Message(3),
        TranscriptRowKind::GoalCompletion, TranscriptRowKind::Message(4),
    ], "goal card follows the final response and precedes the next unrelated turn");
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

    let before = transcript_rows_fingerprint_with_retry(&session, &HashSet::new(), false);
    session.turns[1].started_at = 250;
    let after = transcript_rows_fingerprint_with_retry(&session, &HashSet::new(), false);
    let corrected_rows = folded_transcript_row_kinds_with_retry(&session, &HashSet::new(), false);
    assert_eq!(corrected_rows, vec![
        TranscriptRowKind::Message(0), TranscriptRowKind::Message(1),
        TranscriptRowKind::GoalCompletion, TranscriptRowKind::Message(2),
        TranscriptRowKind::Message(3), TranscriptRowKind::Message(4),
    ], "restored turn boundaries determine which response owns the goal card");
    assert_ne!(before, after, "the cache must refresh when restored boundaries move the card");
    session.turns[1].started_at = 110;

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
    ], "a done goal waits while the selected turn is still running");
    println!("running: {:?}", rows);
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


if __name__ == "__main__":
    main()
