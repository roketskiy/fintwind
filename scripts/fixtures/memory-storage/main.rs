//! The production storage module, exercised through its public API and SQLite
//! side effects. This fixture avoids unrelated provider compilation.
#![allow(dead_code)]

#[path = "../../../crates/fintwind-core/src/attachments.rs"]
mod attachments;
#[path = "../../../crates/fintwind-core/src/blob_store.rs"]
mod blob_store;
#[path = "../../../crates/fintwind-core/src/persistence.rs"]
mod persistence;
use anyhow::{Context, ensure};
pub use fintwind_protocol::DaemonSettings;
use fintwind_protocol::{i18n, identity, model, theme};
use model::{
    BackgroundWorkItem, BackgroundWorkKind, BackgroundWorkSnapshot, BackgroundWorkStatus,
    BackgroundWorkTranscript,
};
use rusqlite::Connection;
use std::{fs, path::PathBuf, sync::Arc};

fn count(connection: &Connection) -> anyhow::Result<i64> {
    Ok(connection.query_row("SELECT count FROM write_count", [], |row| row.get(0))?)
}

fn reopened_snapshot(
    root: &std::path::Path,
    name: &str,
    transcript: Option<fintwind_protocol::provider_session::NativeTranscript>,
) -> anyhow::Result<()> {
    let path = root.join(format!("{name}.db"));
    let mut state = persistence::PersistedState::fresh(root.to_owned());
    let id = state.sessions[0].id;
    let synthetic = transcript.is_none();
    if let Some(transcript) = transcript {
        let session = state.session_mut(id).unwrap();
        session.messages = transcript.messages;
        session.turns = transcript.turns;
        session.transcript_blocks = transcript.blocks;
    } else {
        state.session_mut(id).unwrap().begin_turn("prompt");
        // Equal text can be intentional; snapshot membership is defined by ID.
        for text in ["same answer", "same answer", "final answer"] {
            state
                .session_mut(id)
                .unwrap()
                .push_message(model::MessageRole::Assistant, text);
        }
    }
    for iteration in 0..12 {
        let store = persistence::StateStore::daemon(path.clone());
        for message in &mut state.session_mut(id).unwrap().messages {
            message.id = uuid::Uuid::new_v4();
        }
        if synthetic && iteration == 6 {
            state.session_mut(id).unwrap().messages.swap(1, 2);
        }
        if synthetic && iteration == 9 {
            state.session_mut(id).unwrap().messages.pop();
        }
        state.mark_session_dirty(id);
        store.save(&mut state)?;
        // The same daemon can receive another imported snapshot before restart.
        for message in &mut state.session_mut(id).unwrap().messages {
            message.id = uuid::Uuid::new_v4();
        }
        state.mark_session_dirty(id);
        store.save(&mut state)?;
        let reopened = persistence::StateStore::daemon(path.clone());
        let mut loaded = reopened.load()?;
        reopened.hydrate(loaded.session_mut(id).unwrap())?;
        ensure!(
            serde_json::to_value(&loaded.session_mut(id).unwrap().messages)?
                == serde_json::to_value(&state.session_mut(id).unwrap().messages)?,
            "reopening snapshot {iteration} changed its ordered message set"
        );
    }
    // Reproduce already persisted duplicates left by older versions, including
    // stale IDs absent from this process's write cache.
    let connection = Connection::open(&path)?;
    connection.execute(
        "INSERT INTO messages(id,session_id,turn_id,position,role,content,created_at,streaming,display_content,attachments)
         SELECT ?1,session_id,turn_id,position,role,content,created_at,streaming,display_content,attachments FROM messages WHERE session_id=?2 AND position=1",
        [uuid::Uuid::new_v4().to_string(), id.to_string()],
    )?;
    let store = persistence::StateStore::daemon(path.clone());
    state.mark_session_dirty(id);
    store.save(&mut state)?;
    let mut loaded = store.load()?;
    store.hydrate(loaded.session_mut(id).unwrap())?;
    ensure!(
        serde_json::to_value(&loaded.session_mut(id).unwrap().messages)?
            == serde_json::to_value(&state.session_mut(id).unwrap().messages)?,
        "authoritative snapshot retained legacy duplicate rows"
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("provide a fresh isolated directory")?,
    );
    ensure!(
        root.is_dir() && !root.join("app.db").exists(),
        "refusing non-isolated database"
    );
    let path = root.join("app.db");
    reopened_snapshot(&root, "snapshots", None)?;
    let real_snapshot = if let Some(history) = std::env::args().nth(2) {
        let events: serde_json::Value = serde_json::from_slice(&fs::read(history)?)?;
        let snapshot = events
            .as_array()
            .context("expected history event array")?
            .iter()
            .rev()
            .find_map(|event| event.pointer("/payload/transcript"))
            .context("expected recovered native transcript")?;
        reopened_snapshot(
            &root,
            "real-snapshots",
            Some(serde_json::from_value(snapshot.clone())?),
        )?;
        true
    } else {
        false
    };
    let store = persistence::StateStore::daemon(path.clone());
    let mut state = persistence::PersistedState::fresh(root.clone());
    let id = state.sessions[0].id;
    let session = state.session_mut(id).unwrap();
    session.begin_turn("initial");
    for index in 0..24 {
        let mut item = BackgroundWorkItem::new(
            BackgroundWorkKind::Subagent,
            format!("child-{index}"),
            "child",
            BackgroundWorkStatus::Completed,
        );
        item.output = Some("history".repeat(72 * 1024));
        session
            .background_work
            .push(Arc::new(BackgroundWorkSnapshot {
                item,
                transcript: BackgroundWorkTranscript::default(),
            }));
    }
    store.save(&mut state)?;
    let connection = Connection::open(&path)?;
    connection.execute_batch("CREATE TABLE write_count(count INTEGER NOT NULL); INSERT INTO write_count VALUES(0);
        CREATE TRIGGER count_details AFTER UPDATE ON session_details BEGIN UPDATE write_count SET count = count + 1; END;")?;
    for _ in 0..30 {
        state.mark_session_dirty(id);
        store.save(&mut state)?;
    }
    ensure!(count(&connection)? == 0, "unchanged details were rewritten");
    state
        .session_mut(id)
        .unwrap()
        .messages
        .last_mut()
        .unwrap()
        .content = "message-only-change".into();
    store.save(&mut state)?;
    ensure!(
        count(&connection)? == 0,
        "message-only change rewrote detail"
    );
    let message: String = connection.query_row(
        "SELECT content FROM messages WHERE session_id = ?1 ORDER BY position DESC LIMIT 1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    ensure!(
        message == "message-only-change",
        "message update was skipped"
    );
    state.session_mut(id).unwrap().model = Some("new-model".into());
    state
        .session_mut(id)
        .unwrap()
        .messages
        .last_mut()
        .unwrap()
        .content = "after-rollback".into();
    connection.execute_batch("CREATE TRIGGER fail_message BEFORE UPDATE ON messages BEGIN SELECT RAISE(ABORT, 'controlled rollback'); END;")?;
    ensure!(
        store.save(&mut state).is_err(),
        "controlled write failure did not roll back"
    );
    ensure!(count(&connection)? == 0, "detail update escaped rollback");
    connection.execute_batch("DROP TRIGGER fail_message;")?;
    store.save(&mut state)?;
    ensure!(
        count(&connection)? == 1,
        "rolled-back detail fingerprint suppressed retry"
    );
    let skeleton_store = persistence::StateStore::daemon(path.clone());
    let mut skeletons = skeleton_store.load()?;
    ensure!(
        !skeletons.sessions[0].detail_loaded,
        "load unexpectedly hydrated history"
    );
    skeletons.session_mut(id).unwrap().title = "renamed-skeleton".into();
    skeleton_store.save(&mut skeletons)?;
    ensure!(count(&connection)? == 1, "skeleton overwrote history");
    skeleton_store.hydrate(&mut skeletons.sessions[0])?;
    ensure!(
        skeletons.sessions[0].background_work.len() == 24,
        "skeleton lost child history"
    );
    ensure!(
        skeletons.sessions[0].messages.last().unwrap().content == "after-rollback",
        "retry lost message"
    );
    let retained = state.sessions[0].clone();
    state.sessions.clear();
    store.save(&mut state)?;
    ensure!(
        connection.query_row("SELECT count(*) FROM session_details", [], |row| row
            .get::<_, i64>(0))?
            == 0,
        "deletion retained detail"
    );
    state.push_session(retained);
    store.save(&mut state)?;
    ensure!(
        connection.query_row("SELECT count(*) FROM session_details", [], |row| row
            .get::<_, i64>(0))?
            == 1,
        "deleted fingerprint suppressed a restored detail"
    );
    fs::write(
        root.join("result.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "passed": true, "unchangedDirtySaves": 30, "unchangedDetailUpdates": 0,
            "snapshotReopens": 12, "warmAndColdReplacement": true,
            "legacyDuplicateRemoved": true, "realSnapshotReplayed": real_snapshot,
            "messageOnlyUpdatePersisted": true, "rollbackFingerprintRetried": true,
        "skeletonPreserved24Children": true, "deletedDetailRemoved": true,
        "deletedFingerprintDidNotSuppressRestore": true,
            "productionModule": "crates/fintwind-core/src/persistence.rs"
        }))?,
    )?;
    println!("[OK] {}", root.join("result.json").display());
    Ok(())
}
