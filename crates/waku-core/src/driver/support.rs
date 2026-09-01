//! Helpers shared by the provider drivers: the OpenCode Computer Use
//! configuration, stderr triage, and tool-name classification.

use std::path::Path;

use anyhow::{Context as _, anyhow};
use serde_json::Value;

use super::computer_use as computer_use_runtime;
use crate::driver::DriverEventSender;
use crate::model::ActivityKind;

#[derive(Clone)]
pub(super) struct HeadlessComputerUseConfig {
    pub(super) base: computer_use_runtime::ComputerUseConfig,
    pub(super) config_content: String,
}

pub(super) struct HeadlessComputerUseRuntime {
    runtime: computer_use_runtime::ComputerUseRuntime,
    pub(super) config: HeadlessComputerUseConfig,
}

impl HeadlessComputerUseRuntime {
    pub(super) fn start(events: DriverEventSender) -> anyhow::Result<Self> {
        let runtime = computer_use_runtime::ComputerUseRuntime::start(events)?;
        let existing = match std::env::var("OPENCODE_CONFIG_CONTENT") {
            Ok(content) => Some(content),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(anyhow!("OPENCODE_CONFIG_CONTENT is not valid UTF-8"));
            }
        };
        let base = runtime.config.clone();
        let config_content = build_opencode_computer_use_config(
            existing.as_deref(),
            &base.server_path,
            &base.repl_path,
            &base.skill_path,
            &base.process_directory,
        )?;
        Ok(Self {
            runtime,
            config: HeadlessComputerUseConfig {
                base,
                config_content,
            },
        })
    }

    pub(super) fn stop(&self) {
        self.runtime.stop();
    }
}

fn build_opencode_computer_use_config(
    existing: Option<&str>,
    server_path: &Path,
    repl_path: &Path,
    skill_path: &Path,
    process_directory: &Path,
) -> anyhow::Result<String> {
    let mut config = existing
        .map(serde_json::from_str::<Value>)
        .transpose()
        .context("OPENCODE_CONFIG_CONTENT is invalid JSON")?
        .unwrap_or_else(|| serde_json::json!({}));
    let root = config
        .as_object_mut()
        .ok_or_else(|| anyhow!("OPENCODE_CONFIG_CONTENT must contain a JSON object"))?;
    let mcp = root
        .entry("mcp")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("OPENCODE_CONFIG_CONTENT.mcp must be a JSON object"))?;
    mcp.insert(
        "waku_js_repl".into(),
        serde_json::json!({
            "type": "local",
            "command": [repl_path.display().to_string()],
            "enabled": true,
            "environment": {
                "WAKU_COMPUTER_USE_SERVER": server_path.display().to_string(),
                "WAKU_COMPUTER_USE_PROCESS_DIRECTORY": process_directory.display().to_string(),
            },
        }),
    );
    let instructions = root
        .entry("instructions")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow!("OPENCODE_CONFIG_CONTENT.instructions must be a JSON array"))?;
    let skill_path = skill_path.display().to_string();
    if !instructions
        .iter()
        .any(|instruction| instruction.as_str() == Some(&skill_path))
    {
        instructions.push(Value::String(skill_path));
    }
    serde_json::to_string(&config).context("could not encode OpenCode Computer Use configuration")
}

/// The environment that hands OpenCode its Computer Use configuration.
pub(super) fn opencode_computer_use_environment(
    config: &HeadlessComputerUseConfig,
) -> Vec<(String, String)> {
    vec![
        ("OPENCODE_CONFIG_CONTENT".to_owned(), config.config_content.clone()),
        (
            "WAKU_COMPUTER_USE_SERVER".to_owned(),
            config.base.server_path.display().to_string(),
        ),
        (
            "WAKU_COMPUTER_USE_PROCESS_DIRECTORY".to_owned(),
            config.base.process_directory.display().to_string(),
        ),
    ]
}

pub(super) fn classify_tool(name: &str) -> ActivityKind {
    ActivityKind::from_tool_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_tools_are_plans_not_file_writes() {
        assert_eq!(classify_tool("TodoWrite"), ActivityKind::Plan);
        assert_eq!(classify_tool("todo_write"), ActivityKind::Plan);
        assert_eq!(classify_tool("apply_patch"), ActivityKind::FileChange);
        assert_eq!(classify_tool("read"), ActivityKind::FileRead);
        assert_eq!(classify_tool("ReadFile"), ActivityKind::FileRead);
        assert_eq!(classify_tool("grep"), ActivityKind::FileSearch);
        assert_eq!(classify_tool("glob"), ActivityKind::FileSearch);
        assert_eq!(classify_tool("ls"), ActivityKind::FileList);
        assert_eq!(classify_tool("websearch"), ActivityKind::Search);
        assert_eq!(classify_tool("create_thread"), ActivityKind::Tool);
        assert_eq!(classify_tool("read_mcp_resource"), ActivityKind::Tool);
        assert_eq!(classify_tool("list_threads"), ActivityKind::Tool);
    }

    #[test]
    fn opencode_computer_use_config_preserves_existing_inline_config() {
        let content = build_opencode_computer_use_config(
            Some(
                r#"{
                    "mcp": {
                        "existing": {
                            "type": "local",
                            "command": ["existing-server"],
                            "enabled": true
                        }
                    },
                    "instructions": ["existing.md"],
                    "plugin": ["existing-plugin"]
                }"#,
            ),
            Path::new("/Applications/Waku Computer Use"),
            Path::new("/Applications/Waku.app/Contents/Resources/waku_js_repl"),
            Path::new(
                "/Applications/Waku.app/Contents/Resources/skills/waku-computer-use/SKILL.md",
            ),
            Path::new("/tmp/waku computer use/session"),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            value
                .pointer("/mcp/existing/command/0")
                .and_then(Value::as_str),
            Some("existing-server")
        );
        assert_eq!(
            value
                .pointer("/mcp/waku_js_repl/command/0")
                .and_then(Value::as_str),
            Some("/Applications/Waku.app/Contents/Resources/waku_js_repl")
        );
        assert_eq!(
            value
                .pointer("/mcp/waku_js_repl/environment/WAKU_COMPUTER_USE_SERVER")
                .and_then(Value::as_str),
            Some("/Applications/Waku Computer Use")
        );
        assert_eq!(
            value.get("instructions").and_then(Value::as_array).unwrap(),
            &[
                Value::String("existing.md".into()),
                Value::String(
                    "/Applications/Waku.app/Contents/Resources/skills/waku-computer-use/SKILL.md"
                        .into(),
                ),
            ]
        );
        assert_eq!(
            value.pointer("/plugin/0").and_then(Value::as_str),
            Some("existing-plugin")
        );
        assert!(value.pointer("/mcp/waku_computer_use").is_none());
    }
}
