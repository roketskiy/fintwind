//! Resolve explicit skill references against OpenCode's authoritative registry.
//! All callers run on daemon workers: neither registry I/O nor discovery may
//! enter the UI render path. The server owns content, precedence and permissions.

use std::collections::HashSet;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};

use fintwind_workspace::composer_complete::{CommandScope, SlashCommand};
use fintwind_protocol::model::ReportedCommand;
use crate::opencode_session::OpenCodeServer;

#[derive(Deserialize)]
struct RegisteredSkill {
    id: String,
    #[serde(default)]
    description: Option<String>,
}

fn registered_skills(
    server: &OpenCodeServer,
    directory: &str,
) -> anyhow::Result<Vec<RegisteredSkill>> {
    let url = url::Url::parse_with_params(
        "http://localhost/api/skill",
        &[("location[directory]", directory)],
    )?;
    let path = format!("/api/skill?{}", url.query().unwrap_or_default());
    // Merely listing skills does not initialize a cold location's configured
    // sources. This is also required when sending before the UI index arrives.
    let config_path = format!("/api/config?{}", url.query().unwrap_or_default());
    server
        .request_for_directory_with_timeout(
            directory,
            "GET",
            &config_path,
            None,
            Duration::from_secs(15),
        )
        .context("could not load the OpenCode skill configuration")?;
    let response = server
        .request_for_directory_with_timeout(directory, "GET", &path, None, Duration::from_secs(15))
        .context("could not resolve OpenCode skills")?;
    serde_json::from_value(response.get("data").cloned().unwrap_or(Value::Null))
        .context("OpenCode returned an invalid skill registry")
}

/// Use IDs rather than frontmatter display names, including built-in,
/// configured, flat-file, nested and non-autoinvoked skills.
pub fn discover_commands(
    binary: &Path,
    directory: &Path,
) -> anyhow::Result<Vec<SlashCommand>> {
    let server = crate::opencode_pool::acquire(binary, directory)?;
    let directory = crate::opencode_session::resolve_request_directory(directory);
    Ok(registered_skills(&server, &directory)?
        .into_iter()
        .map(|skill| SlashCommand {
            name: skill.id,
            description: skill.description.unwrap_or_default(),
            scope: CommandScope::Skill,
            argument_hint: None,
            template: None,
        })
        .collect())
}

/// Add native attachments to both request shapes. An older server must reject
/// an unsupported skill field rather than silently send an unloaded skill as
/// plain text. Resolve at dispatch so edits, queued prompts and steers all use
/// the current workspace's definitions, never a stale UI index.
pub fn attach(
    server: &OpenCodeServer,
    directory: &str,
    text: &str,
    current: &mut Value,
    legacy: &mut Value,
) -> anyhow::Result<()> {
    if !text.contains('@') && !text.lines().any(|line| line.starts_with('/')) {
        return Ok(());
    }
    let skills = registered_skills(server, directory)?;
    let mut seen = HashSet::new();
    let mut attachments = Vec::new();
    for (start, sigil) in text.char_indices() {
        if sigil != '@' && (sigil != '/' || (start != 0 && !text[..start].ends_with('\n'))) {
            continue;
        }
        let rest = &text[start + 1..];
        let matched = skills
            .iter()
            .filter(|skill| !skill.id.is_empty())
            .filter(|skill| {
                rest.strip_prefix(&skill.id).is_some_and(|tail| {
                    tail.chars().next().is_none_or(|next| {
                        next.is_whitespace()
                            || ",;:!?)]}\u{ff0c}\u{3002}\u{ff01}\u{ff1f}\u{ff1b}\u{ff1a}"
                                .contains(next)
                    })
                })
            })
            .max_by_key(|skill| skill.id.len());
        if let Some(skill) = matched
            && seen.insert(&skill.id) {
                attachments.push(json!({"id": skill.id}));
            }
    }
    if !attachments.is_empty() {
        current["skills"] = json!(attachments);
        legacy["skills"] = current["skills"].clone();
    }
    Ok(())
}

/// The commands the OpenCode server itself has loaded: markdown files,
/// `opencode.json` entries, and everything plugins registered. The server
/// registry is the only view that can see those last two, so it folds into
/// filesystem discovery ([`merge_reported_commands`]) rather than replacing
/// it — a discovered entry keeps its template, which Fintwind expands at
/// submit because the server transport takes plain prompt text.
///
/// The pooled server builds the registry asynchronously: naming the location
/// on `/api/config` starts the workspace load, built-ins can surface first,
/// and the project's own commands join later. So this triggers the load and
/// polls `/api/command` until two consecutive polls agree — including two
/// agreeing empty polls, which is a workspace with no commands at all — on a
/// bounded budget. An unreachable server degrades to an empty report and
/// discovery stands alone. Blocking (it may start the resident process), so
/// callers must already run off the UI thread.
pub fn server_commands(binary: &Path, directory: &Path) -> Vec<ReportedCommand> {
    const COMMAND_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
    const COMMAND_DISCOVERY_BUDGET: Duration = Duration::from_secs(30);
    const COMMAND_DISCOVERY_INITIAL_DELAY: Duration = Duration::from_millis(500);
    const COMMAND_DISCOVERY_MAX_DELAY: Duration = Duration::from_secs(2);
    let Ok(server) = crate::opencode_pool::acquire(binary, directory) else {
        return Vec::new();
    };
    // The pooled server runs in its own data directory; the workspace rides
    // along as normalized per-request location data (see the model catalog).
    let location = crate::opencode_session::resolve_request_directory(directory);
    // `location[directory]` percent-encodes to the deepObject query form the
    // API documents, which both routes accept.
    let query = |route: &str| {
        url::Url::parse_with_params(
            &format!("http://localhost{route}"),
            &[("location[directory]", location.as_str())],
        )
        .ok()
        .and_then(|url| url.query().map(|query| format!("{route}?{query}")))
        .unwrap_or_else(|| route.to_owned())
    };
    // Naming the location here starts the workspace load; the response body
    // itself is not needed.
    let _ = server.request_for_directory_with_timeout(
        &location,
        "GET",
        &query("/api/config"),
        None,
        COMMAND_REQUEST_TIMEOUT,
    );
    let started = Instant::now();
    let mut delay = COMMAND_DISCOVERY_INITIAL_DELAY;
    // The registry is built asynchronously after the config scan, and it can
    // surface OpenCode's built-ins before the project's own commands join.
    // The load is complete when two consecutive polls agree — including two
    // agreeing empty polls, which is a workspace with no commands at all.
    let mut previous: Option<Vec<ReportedCommand>> = None;
    let mut stable_polls = 0;
    loop {
        let parsed = server
            .request_for_directory_with_timeout(
                &location,
                "GET",
                &query("/api/command"),
                None,
                COMMAND_REQUEST_TIMEOUT,
            )
            .ok()
            .map(|value| {
                value
                    .get("data")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.get("name")?.as_str()?;
                        Some(ReportedCommand {
                            name: name.to_owned(),
                            description: entry
                                .get("description")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        })
                    })
                    .collect::<Vec<_>>()
            });
        stable_polls = match (&previous, &parsed) {
            (Some(previous), Some(parsed)) if previous == parsed => stable_polls + 1,
            _ => 0,
        };
        let settled = parsed.is_some() && stable_polls >= 2;
        if settled || started.elapsed() >= COMMAND_DISCOVERY_BUDGET {
            return parsed.unwrap_or_default();
        }
        previous = parsed;
        thread::sleep(delay);
        delay = (delay * 2).min(COMMAND_DISCOVERY_MAX_DELAY);
    }
}


pub fn discover_live_commands(
    binary: &Path,
    project_root: &Path,
) -> (Vec<ReportedCommand>, Option<Vec<SlashCommand>>) {
    let reported = server_commands(binary, project_root);
    let skills = discover_commands(binary, project_root).ok();
    (reported, skills)
}
