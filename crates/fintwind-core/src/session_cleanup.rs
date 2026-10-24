//! Retention cleanup uses OpenCode's public session API; its database stays private.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::persistence::{PersistedState, StateStore};
use fintwind_protocol::model::AgentSession;
use fintwind_protocol::session_cleanup::SessionCleanupOutcome;

pub trait SessionSource {
    fn request(&self, method: &str, path: &str) -> Result<Value>;
}

impl SessionSource for crate::opencode_session::OpenCodeServer {
    fn request(&self, method: &str, path: &str) -> Result<Value> {
        self.request(method, path, None)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeSession {
    id: String,
    #[serde(rename = "parentID", alias = "parentId")]
    parent_id: Option<String>,
    time: NativeTime,
}

#[derive(Deserialize)]
struct NativeTime {
    created: u64,
    updated: u64,
}

struct Inventory {
    sessions: HashMap<String, NativeSession>,
    active: HashSet<String>,
}

impl Inventory {
    fn read(source: &impl SessionSource) -> Result<Self> {
        let mut sessions = HashMap::new();
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        for page in 0..50 {
            let mut path = "/api/session?limit=500".to_owned();
            if let Some(cursor) = &cursor {
                path.push_str("&cursor=");
                path.push_str(&crate::opencode_session::encode_path_segment(cursor));
            }
            let response = source.request("GET", &path)?;
            let rows = response
                .get("data")
                .and_then(Value::as_array)
                .context("OpenCode returned an invalid session list")?;
            for row in rows {
                let session: NativeSession = serde_json::from_value(row.clone())?;
                ensure!(
                    !session.id.is_empty(),
                    "OpenCode returned an empty session ID"
                );
                sessions.insert(session.id.clone(), session);
            }
            let next = response.pointer("/cursor/next").and_then(Value::as_str);
            match next {
                Some(next) if !rows.is_empty() => {
                    ensure!(
                        page < 49 && cursors.insert(next.to_owned()),
                        "Incomplete OpenCode session listing"
                    );
                    cursor = Some(next.to_owned());
                }
                _ => {
                    ensure!(
                        rows.len() < 500 || response.get("cursor").is_some(),
                        "Missing OpenCode session cursor"
                    );
                    let active = source.request("GET", "/api/session/active")?;
                    let active = active
                        .get("data")
                        .and_then(Value::as_object)
                        .context("OpenCode returned an invalid active-session list")?
                        .keys()
                        .cloned()
                        .collect();
                    return Ok(Self { sessions, active });
                }
            }
        }
        bail!("Incomplete OpenCode session listing")
    }

    fn tree(&self, root: &str) -> HashSet<String> {
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for session in self.sessions.values() {
            if let Some(parent) = &session.parent_id {
                children.entry(parent).or_default().push(&session.id);
            }
        }
        let mut tree = HashSet::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if tree.insert(id.to_owned()) {
                pending.extend(children.get(id).into_iter().flatten().copied());
            }
        }
        tree
    }

    fn eligible(
        &self,
        state: &PersistedState,
        blocked: &HashSet<Uuid>,
        id: Uuid,
        before: u64,
    ) -> Option<HashSet<String>> {
        let session = state.sessions.iter().find(|session| session.id == id)?;
        if !local_eligible(session, before) || blocked.contains(&id) {
            return None;
        }
        let native = session.native_session_id.as_deref()?;
        let tree = self.tree(native);
        if tree.iter().any(|id| {
            self.active.contains(id)
                || self.sessions.get(id).is_some_and(|row| {
                    row.time.updated == 0 || row.time.updated.max(row.time.created) / 1000 >= before
                })
        }) {
            return None;
        }
        if state.sessions.iter().any(|row| {
            row.native_session_id
                .as_ref()
                .is_some_and(|id| tree.contains(id))
                && (!local_eligible(row, before) || blocked.contains(&row.id))
        }) {
            return None;
        }
        Some(tree)
    }
}

fn local_eligible(session: &AgentSession, before: u64) -> bool {
    session.provider == "opencode"
        && !session.archived
        && session.native_session_id.is_some()
        && !session.status.is_busy()
        && session.queued_messages.is_empty()
        && !fintwind_protocol::submission::has_unsettled_submissions(session)
        && session.updated_at > 0
        && session
            .updated_at
            .max(session.created_at)
            .max(session.last_reply_at.unwrap_or(0))
            < before
}

fn details_eligible(
    state: &PersistedState,
    store: &StateStore,
    tree: &HashSet<String>,
    before: u64,
) -> Result<bool> {
    for session in &state.sessions {
        if !session
            .native_session_id
            .as_ref()
            .is_some_and(|id| tree.contains(id))
        {
            continue;
        }
        // Load one cold transcript at a time; never retain all old histories
        // just to check pending inputs in the detail blob.
        if !session.detail_loaded {
            let mut detail = session.clone();
            store.hydrate(&mut detail)?;
            if !local_eligible(&detail, before) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub fn preview(
    state: &PersistedState,
    store: &StateStore,
    blocked: &HashSet<Uuid>,
    visible: &[Uuid],
    before: u64,
    source: &impl SessionSource,
) -> Result<Vec<Uuid>> {
    let inventory = Inventory::read(source)?;
    let visible: HashSet<_> = visible.iter().copied().collect();
    let mut result = Vec::new();
    for row in state
        .sessions
        .iter()
        .filter(|row| visible.contains(&row.id))
    {
        if let Some(tree) = inventory.eligible(state, blocked, row.id, before)
            && details_eligible(state, store, &tree, before)?
        {
            result.push(row.id);
        }
    }
    Ok(result)
}

/// Call with the daemon's admission gate and task-state lock held. A failed
/// provider deletion preserves local identity so the same request can retry.
pub fn clean(
    state: &mut PersistedState,
    store: &StateStore,
    blocked: &HashSet<Uuid>,
    id: Uuid,
    before: u64,
    source: &impl SessionSource,
) -> Result<SessionCleanupOutcome> {
    if !state
        .sessions
        .iter()
        .any(|row| row.id == id && local_eligible(row, before))
    {
        return Ok(SessionCleanupOutcome::Skipped);
    }
    let inventory = Inventory::read(source)?;
    let Some(tree) = inventory.eligible(state, blocked, id, before) else {
        return Ok(SessionCleanupOutcome::Skipped);
    };
    if !details_eligible(state, store, &tree, before)? {
        return Ok(SessionCleanupOutcome::Skipped);
    }
    // Re-read every affected row immediately before the recursive official delete.
    for native in &tree {
        let path = format!(
            "/api/session/{}",
            crate::opencode_session::encode_path_segment(native)
        );
        match source.request("GET", &path) {
            Ok(response) => {
                let row: NativeSession =
                    serde_json::from_value(response.get("data").unwrap_or(&response).clone())?;
                ensure!(row.id == *native, "OpenCode returned a different session");
                if row.time.updated == 0 || row.time.updated.max(row.time.created) / 1000 >= before
                {
                    return Ok(SessionCleanupOutcome::Skipped);
                }
            }
            Err(error) if error.to_string().contains("HTTP 404") => {}
            Err(error) => return Err(error),
        }
    }
    let active = source.request("GET", "/api/session/active")?;
    let active = active
        .get("data")
        .and_then(Value::as_object)
        .context("Invalid active-session list")?;
    if tree.iter().any(|native| active.contains_key(native)) {
        return Ok(SessionCleanupOutcome::Skipped);
    }
    let native = state
        .sessions
        .iter()
        .find(|row| row.id == id)
        .and_then(|row| row.native_session_id.as_ref())
        .context("Missing native session")?;
    let path = format!(
        "/api/session/{}",
        crate::opencode_session::encode_path_segment(native)
    );
    match source.request("DELETE", &path) {
        Ok(_) => {}
        Err(error) if error.to_string().contains("HTTP 404") => {}
        Err(error) => return Err(error),
    }
    let deleted: Vec<_> = state
        .sessions
        .iter()
        .filter(|row| {
            row.native_session_id
                .as_ref()
                .is_some_and(|id| tree.contains(id))
        })
        .map(|row| row.id)
        .collect();
    let mut next = state.clone();
    next.sessions.retain(|row| !deleted.contains(&row.id));
    let used: HashSet<_> = next.sessions.iter().map(|row| row.project_id).collect();
    next.projects
        .retain(|row| !row.is_projectless() || used.contains(&row.id));
    store.save(&mut next)?;
    *state = next;
    Ok(SessionCleanupOutcome::Deleted {
        session_ids: deleted,
    })
}
