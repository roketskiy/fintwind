//! Resolve explicit skill references against OpenCode's authoritative registry.
//! All callers run on daemon workers: neither registry I/O nor discovery may
//! enter the UI render path. The server owns content, precedence and permissions.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::composer_complete::{CommandScope, SlashCommand};
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
pub(crate) fn discover_commands(
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
pub(crate) fn attach(
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
        if let Some(skill) = matched {
            if seen.insert(&skill.id) {
                attachments.push(json!({"id": skill.id}));
            }
        }
    }
    if !attachments.is_empty() {
        current["skills"] = json!(attachments);
        legacy["skills"] = current["skills"].clone();
    }
    Ok(())
}
