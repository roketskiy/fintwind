//! Session preference/persistence black box; no services or application launched.
//! Failure cases and immutable-version contract: docs/mermaid-prompt.md.

use anyhow::{Context, Result, ensure};
use fintwind_client::persistence::{AppSettings, PersistedState as ClientState};
use fintwind_core::persistence::{PersistedState, StateStore};
use fintwind_protocol::model::{
    AgentSession, MERMAID_PROMPT_VERSION, ProviderResumeCursor, TurnStatus,
};
use serde_json::json;
use std::path::PathBuf;
use uuid::Uuid;

fn check(checks: &mut Vec<&'static str>, name: &'static str, valid: bool) -> Result<()> {
    ensure!(valid, "{name}");
    checks.push(name);
    Ok(())
}

fn main() -> Result<()> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("pass an artifact directory")?,
    );
    let output = root.join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&output)?;
    let report = output.join("report.json");
    std::fs::write(&report, br#"{"passed":false,"stage":"running"}"#)?;
    let mut checks = Vec::new();
    let mut defaults = ClientState::empty();
    let project = Uuid::new_v4();
    let old = defaults.new_session(project);
    check(
        &mut checks,
        "default_off",
        !defaults.mermaid_visualization_enabled && old.mermaid_prompt_version.is_none(),
    )?;
    defaults.mermaid_visualization_enabled = true;
    let mut enabled = defaults.new_session(project);
    check(
        &mut checks,
        "old_empty_session_not_retrofitted",
        old.mermaid_prompt_version.is_none(),
    )?;
    check(
        &mut checks,
        "new_session_freezes_version",
        enabled.mermaid_prompt_version.as_deref() == Some(MERMAID_PROMPT_VERSION),
    )?;
    enabled.begin_turn("Fixture first request");
    defaults.mermaid_visualization_enabled = false;
    check(
        &mut checks,
        "running_session_unchanged",
        enabled.mermaid_prompt_version.as_deref() == Some(MERMAID_PROMPT_VERSION),
    )?;
    let future_off = defaults.new_session(project);
    check(
        &mut checks,
        "later_session_uses_new_default",
        future_off.mermaid_prompt_version.is_none(),
    )?;
    enabled.finish_active_turn(TurnStatus::Completed);
    let fork = enabled
        .fork_through_turn(
            1,
            ProviderResumeCursor::from_session_id("fixture-fork".into()),
            "Fork",
        )
        .context("fork fixture turn")?;
    check(
        &mut checks,
        "fork_inherits_parent_not_default",
        fork.mermaid_prompt_version == enabled.mermaid_prompt_version,
    )?;
    check(
        &mut checks,
        "list_projection_preserves_version",
        enabled.list_projection().mermaid_prompt_version == enabled.mermaid_prompt_version,
    )?;

    let app: AppSettings = serde_json::from_value(json!({}))?;
    check(
        &mut checks,
        "legacy_settings_default_off",
        !app.mermaid_visualization_enabled,
    )?;
    defaults.mermaid_visualization_enabled = true;
    let saved: ClientState = serde_json::from_slice(&serde_json::to_vec(&defaults)?)?;
    check(
        &mut checks,
        "global_default_roundtrip",
        saved.mermaid_visualization_enabled,
    )?;
    let mut legacy = serde_json::to_value(&enabled)?;
    legacy
        .as_object_mut()
        .context("session object")?
        .remove("mermaid_prompt_version");
    let legacy: AgentSession = serde_json::from_value(legacy)?;
    check(
        &mut checks,
        "legacy_session_default_off",
        legacy.mermaid_prompt_version.is_none(),
    )?;

    let workspace = output.join("workspace");
    std::fs::create_dir(&workspace)?;
    let mut state = PersistedState::fresh(workspace);
    let project_id = state.projects[0].id;
    state.sessions.clear();
    let mut on = enabled.clone();
    on.project_id = project_id;
    let on_id = on.id;
    let mut off = old.clone();
    off.project_id = project_id;
    off.begin_turn("Fixture off-session request");
    off.finish_active_turn(TurnStatus::Completed);
    let off_id = off.id;
    state.push_session(on);
    state.push_session(off);
    state.selected_session = Some(on_id);
    let database = output.join("sessions.db");
    let store = StateStore::daemon(database.clone());
    store.save(&mut state)?;
    drop(store);
    let store = StateStore::daemon(database);
    let loaded = store.load()?;
    let mut on = loaded
        .sessions
        .iter()
        .find(|session| session.id == on_id)
        .context("saved enabled session")?
        .clone();
    let off = loaded
        .sessions
        .iter()
        .find(|session| session.id == off_id)
        .context("saved off session")?;
    check(
        &mut checks,
        "sqlite_list_snapshot_enabled",
        !on.detail_loaded && on.mermaid_prompt_version.as_deref() == Some(MERMAID_PROMPT_VERSION),
    )?;
    check(
        &mut checks,
        "sqlite_list_snapshot_off",
        off.mermaid_prompt_version.is_none(),
    )?;
    store.hydrate(&mut on)?;
    check(
        &mut checks,
        "sqlite_hydration_preserves_choice",
        on.detail_loaded && on.mermaid_prompt_version.as_deref() == Some(MERMAID_PROMPT_VERSION),
    )?;
    std::fs::write(
        &report,
        serde_json::to_vec_pretty(&json!({
            "passed": true, "checks": checks, "scope": "client defaults, frozen session metadata, fork and isolated SQLite persistence; no UI or provider request",
        }))?,
    )?;
    println!("[OK] {} checks; {}", checks.len(), report.display());
    Ok(())
}
