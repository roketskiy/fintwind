//! Exercises retention cleanup against the real Fintwind SQLite store and an
//! isolated API fixture. No services, user databases, or child processes.

use anyhow::{Context, Result, bail, ensure};
use fintwind_core::{
    persistence::{PersistedState, StateStore},
    session_cleanup::{self, SessionSource},
};
use fintwind_protocol::{
    model::{AgentSession, Message, MessageRole, Project, QueuedMessage, SessionStatus},
    session_cleanup::SessionCleanupOutcome,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    path::PathBuf,
};
use uuid::Uuid;

struct Provider {
    db: Connection,
    active: RefCell<HashSet<String>>,
    fail_delete: Cell<bool>,
    bad_page: Cell<bool>,
    requests: RefCell<Vec<(String, String)>>,
}

impl Provider {
    fn new() -> Result<Self> {
        let db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY, data TEXT NOT NULL)")?;
        Ok(Self {
            db,
            active: RefCell::default(),
            fail_delete: Cell::new(false),
            bad_page: Cell::new(false),
            requests: RefCell::default(),
        })
    }
    fn add(&self, id: &str, parent: Option<&str>, updated: u64) -> Result<()> {
        let row =
            json!({"id":id, "parentID":parent, "time":{"created":1,"updated":updated * 1000}});
        self.db.execute(
            "INSERT OR REPLACE INTO sessions VALUES(?1,?2)",
            params![id, row.to_string()],
        )?;
        Ok(())
    }
    fn exists(&self, id: &str) -> bool {
        self.db
            .query_row("SELECT COUNT(*) FROM sessions WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
            == 1
    }
}

impl SessionSource for Provider {
    fn request(&self, method: &str, path: &str) -> Result<Value> {
        self.requests
            .borrow_mut()
            .push((method.into(), path.into()));
        if path.starts_with("/api/session?") {
            if self.bad_page.get() {
                return Ok(json!({"data":null}));
            }
            let mut query = self.db.prepare("SELECT data FROM sessions ORDER BY id")?;
            let rows = query
                .query_map([], |row| row.get::<_, String>(0))?
                .map(|row| Ok(serde_json::from_str::<Value>(&row?)?))
                .collect::<Result<Vec<_>>>()?;
            return Ok(json!({"data":rows, "cursor":{"next":null}}));
        }
        if path == "/api/session/active" {
            let mut active = serde_json::Map::new();
            for id in self.active.borrow().iter() {
                active.insert(id.clone(), json!({"type":"busy"}));
            }
            return Ok(json!({"data":active}));
        }
        let id = path
            .strip_prefix("/api/session/")
            .context("unexpected API path")?;
        if !self.exists(id) {
            bail!("HTTP 404");
        }
        match method {
            "GET" => {
                let row: String =
                    self.db
                        .query_row("SELECT data FROM sessions WHERE id=?1", [id], |row| {
                            row.get(0)
                        })?;
                Ok(json!({"data":serde_json::from_str::<Value>(&row)?}))
            }
            "DELETE" => {
                if self.fail_delete.get() {
                    bail!("HTTP 503");
                }
                self.db.execute("WITH RECURSIVE tree(id) AS (SELECT ?1 UNION SELECT s.id FROM sessions s JOIN tree ON json_extract(s.data,'$.parentID')=tree.id) DELETE FROM sessions WHERE id IN tree", [id])?;
                Ok(Value::Null)
            }
            _ => bail!("Unexpected method"),
        }
    }
}

fn old(project: Uuid, native: &str, at: u64) -> AgentSession {
    let mut row = AgentSession::new(project);
    row.native_session_id = Some(native.to_owned());
    row.created_at = 1;
    row.updated_at = at;
    row.last_reply_at = Some(at);
    row.messages
        .push(Message::new(MessageRole::User, "fixture history"));
    row
}

fn main() -> Result<()> {
    let base = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("pass an isolated artifact directory")?,
    );
    let root = base.join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&root)?;
    let path = root.join("tasks.db");
    let store = StateStore::new(path.clone());
    let project = Project::from_path(root.clone());
    let before = 1_000_000;
    let mut state = PersistedState::empty();
    state.projects.push(project.clone());
    let provider = Provider::new()?;
    let mut rows = Vec::new();
    for (name, time) in [
        ("old", 1),
        ("recent", before),
        ("busy", 1),
        ("archived", 1),
        ("blocked", 1),
        ("with-child", 1),
        ("with-active-child", 1),
        ("retry", 1),
        ("db-failure", 1),
        ("untracked-local", 1),
        ("queued", 1),
    ] {
        let mut row = old(project.id, name, time);
        if name == "busy" {
            row.status = SessionStatus::Working;
        }
        if name == "archived" {
            row.archived = true;
        }
        if name == "queued" {
            row.queued_messages
                .push(QueuedMessage::new("pending input"));
        }
        provider.add(name, None, time)?;
        rows.push(row);
    }
    provider.add("new-child", Some("with-child"), before)?;
    provider.add("active-child", Some("with-active-child"), 1)?;
    provider.active.borrow_mut().insert("active-child".into());
    provider.add("outside-fintwind", None, 1)?;
    let id = |name: &str| {
        rows.iter()
            .find(|r| r.native_session_id.as_deref() == Some(name))
            .unwrap()
            .id
    };
    state.sessions = rows.clone();
    for row in &rows {
        state.mark_session_dirty(row.id);
    }
    store.save(&mut state)?;
    // Cold startup paths must inspect queued submissions, not empty skeleton fields.
    state = store.load()?;
    let blocked = HashSet::from([id("blocked")]);
    let visible: Vec<_> = rows
        .iter()
        .filter(|row| row.id != id("untracked-local"))
        .map(|row| row.id)
        .collect();
    let preview = session_cleanup::preview(&state, &store, &blocked, &visible, before, &provider)?;
    ensure!(
        preview.iter().copied().collect::<HashSet<_>>()
            == HashSet::from([id("old"), id("retry"), id("db-failure")]),
        "preview includes only eligible visible sessions"
    );
    ensure!(
        provider
            .requests
            .borrow()
            .iter()
            .all(|(method, _)| method == "GET"),
        "preview must be read-only"
    );
    let mut checks = vec![
        "preview_scope_and_age",
        "busy_and_archived_protection",
        "recent_and_active_children",
        "cold_queued_input_protection",
        "read_only_preview",
    ];

    let outcome =
        session_cleanup::clean(&mut state, &store, &blocked, id("old"), before, &provider)?;
    ensure!(
        matches!(outcome, SessionCleanupOutcome::Deleted { session_ids } if session_ids == vec![id("old")])
    );
    ensure!(!provider.exists("old") && !store.load()?.sessions.iter().any(|r| r.id == id("old")));
    let db = Connection::open(&path)?;
    for table in ["sessions", "session_details", "messages", "submissions"] {
        let key = if table == "sessions" {
            "id"
        } else {
            "session_id"
        };
        let count: i64 = db.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE {key}=?1"),
            [id("old").to_string()],
            |r| r.get(0),
        )?;
        ensure!(count == 0, "old data remains in {table}");
    }
    ensure!(matches!(
        session_cleanup::clean(&mut state, &store, &blocked, id("old"), before, &provider)?,
        SessionCleanupOutcome::Skipped
    ));
    checks.extend([
        "both_databases_deleted",
        "transcript_and_submission_rows_removed",
        "repeat_is_noop",
    ]);

    provider.fail_delete.set(true);
    ensure!(
        session_cleanup::clean(&mut state, &store, &blocked, id("retry"), before, &provider)
            .is_err()
    );
    ensure!(provider.exists("retry") && store.load()?.sessions.iter().any(|r| r.id == id("retry")));
    provider.fail_delete.set(false);
    session_cleanup::clean(&mut state, &store, &blocked, id("retry"), before, &provider)?;
    ensure!(
        !provider.exists("retry") && !store.load()?.sessions.iter().any(|r| r.id == id("retry"))
    );
    checks.extend(["provider_failure_preserves_local", "provider_failure_retry"]);

    db.execute_batch("CREATE TRIGGER fail_cleanup BEFORE DELETE ON sessions BEGIN SELECT RAISE(ABORT,'fixture disk failure'); END")?;
    ensure!(
        session_cleanup::clean(
            &mut state,
            &store,
            &blocked,
            id("db-failure"),
            before,
            &provider
        )
        .is_err()
    );
    ensure!(
        !provider.exists("db-failure")
            && store
                .load()?
                .sessions
                .iter()
                .any(|r| r.id == id("db-failure"))
    );
    db.execute_batch("DROP TRIGGER fail_cleanup")?;
    session_cleanup::clean(
        &mut state,
        &store,
        &blocked,
        id("db-failure"),
        before,
        &provider,
    )?;
    ensure!(
        !store
            .load()?
            .sessions
            .iter()
            .any(|r| r.id == id("db-failure"))
    );
    checks.push("local_commit_failure_retries_after_provider_404");

    provider.add("with-child", None, before)?;
    ensure!(matches!(
        session_cleanup::clean(
            &mut state,
            &store,
            &blocked,
            id("with-child"),
            before,
            &provider
        )?,
        SessionCleanupOutcome::Skipped
    ));
    ensure!(provider.exists("with-child"));
    checks.push("recheck_provider_after_preview");
    for name in ["busy", "archived", "blocked", "with-active-child", "queued"] {
        ensure!(matches!(
            session_cleanup::clean(&mut state, &store, &blocked, id(name), before, &provider)?,
            SessionCleanupOutcome::Skipped
        ));
        ensure!(provider.exists(name));
    }
    ensure!(provider.exists("outside-fintwind") && provider.exists("untracked-local"));
    checks.push("unrelated_sessions_preserved");
    // A malformed successful response must never be treated as an empty store.
    provider.bad_page.set(true);
    let calls = provider
        .requests
        .borrow()
        .iter()
        .filter(|(method, _)| method == "DELETE")
        .count();
    ensure!(
        session_cleanup::preview(&state, &store, &blocked, &visible, before, &provider).is_err()
    );
    ensure!(
        session_cleanup::clean(
            &mut state,
            &store,
            &blocked,
            id("untracked-local"),
            before,
            &provider
        )
        .is_err()
    );
    ensure!(
        provider
            .requests
            .borrow()
            .iter()
            .filter(|(method, _)| method == "DELETE")
            .count()
            == calls
    );
    checks.push("malformed_listing_fails_without_delete");
    provider.bad_page.set(false);
    let report = root.join("report.json");
    std::fs::write(
        &report,
        serde_json::to_vec_pretty(&json!({"passed":true,"checks":checks}))?,
    )?;
    println!("{} checks passed: {}", checks.len(), report.display());
    Ok(())
}
