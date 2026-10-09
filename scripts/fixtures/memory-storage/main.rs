//! The production storage module, exercised through its public API and SQLite
//! side effects. This fixture avoids unrelated provider/remote-host compilation.
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
            "messageOnlyUpdatePersisted": true, "rollbackFingerprintRetried": true,
        "skeletonPreserved24Children": true, "deletedDetailRemoved": true,
        "deletedFingerprintDidNotSuppressRestore": true,
            "productionModule": "crates/fintwind-core/src/persistence.rs"
        }))?,
    )?;
    println!("[OK] {}", root.join("result.json").display());
    Ok(())
}
