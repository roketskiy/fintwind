use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

#[derive(Default)]
struct Run {
    parts: HashMap<String, BTreeMap<u64, String>>,
    steps_seen: HashSet<String>,
    calls: HashMap<String, (String, Option<Value>)>,
    calls_seen: HashSet<String>,
    last_text: Option<String>,
    text_count: u8,
    last_tool: Option<(String, Value, bool, Value)>,
    tool_count: u8,
    triggered: bool,
}

#[derive(Default)]
pub(super) struct Watchdog {
    runs: HashMap<String, Run>,
}

impl Watchdog {
    pub(super) fn clear(&mut self, session: &str) {
        self.runs.remove(session);
    }

    /// Returns true exactly once per execution when a complete reply or a
    /// completed tool call has repeated ten times. Event IDs are not content.
    pub(super) fn observe(&mut self, event: &Value, enabled: bool) -> bool {
        if !enabled {
            self.runs.clear();
            return false;
        }
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return false;
        };
        let payload = event.get("data").or_else(|| event.get("properties"));
        let Some(payload) = payload else { return false };
        let Some(session) = payload.get("sessionID").and_then(Value::as_str) else {
            return false;
        };
        if matches!(
            kind,
            "session.execution.succeeded"
                | "session.execution.failed"
                | "session.execution.interrupted"
        ) {
            self.runs.remove(session);
            return false;
        }
        let run = self.runs.entry(session.to_owned()).or_default();
        if run.triggered {
            return false;
        }
        match kind {
            "session.text.ended" => {
                if let (Some(id), Some(ordinal), Some(text)) = (
                    payload.get("assistantMessageID").and_then(Value::as_str),
                    payload.get("ordinal").and_then(Value::as_u64),
                    payload.get("text").and_then(Value::as_str),
                ) && !run.steps_seen.contains(id)
                {
                    run.parts
                        .entry(id.to_owned())
                        .or_default()
                        .insert(ordinal, text.to_owned());
                }
            }
            "session.step.ended" => {
                if let Some(id) = payload.get("assistantMessageID").and_then(Value::as_str)
                    && run.steps_seen.insert(id.to_owned())
                {
                    let text = run
                        .parts
                        .remove(id)
                        .unwrap_or_default()
                        .into_values()
                        .collect::<String>();
                    if !text.is_empty() {
                        run.text_count = if run.last_text.as_ref() == Some(&text) {
                            run.text_count.saturating_add(1)
                        } else {
                            1
                        };
                        run.last_text = Some(text);
                        run.triggered = run.text_count >= 10;
                    } else {
                        run.last_text = None;
                        run.text_count = 0;
                    }
                }
            }
            "session.step.failed" => {
                if let Some(id) = payload.get("assistantMessageID").and_then(Value::as_str)
                    && run.steps_seen.insert(id.to_owned())
                {
                    run.parts.remove(id);
                    run.last_text = None;
                    run.text_count = 0;
                }
            }
            "session.tool.input.started" => {
                if let (Some(id), Some(name)) = (
                    payload.get("id").and_then(Value::as_str),
                    payload.get("name").and_then(Value::as_str),
                ) && !run.calls_seen.contains(id)
                {
                    run.calls
                        .entry(id.to_owned())
                        .or_insert((name.to_owned(), None));
                }
            }
            "session.tool.called" => {
                if let Some(id) = payload.get("id").and_then(Value::as_str)
                    && let Some(call) = run.calls.get_mut(id)
                    && let Some(input) = payload.get("input")
                {
                    call.1 = Some(input.clone());
                }
            }
            "session.tool.success" | "session.tool.error" | "session.tool.failed" => {
                if let Some(id) = payload.get("id").and_then(Value::as_str)
                    && run.calls_seen.insert(id.to_owned())
                    && let Some((name, input)) = run.calls.remove(id)
                {
                    let failed = kind != "session.tool.success";
                    let input = input.or_else(|| payload.get("input").cloned());
                    let output = if failed {
                        payload.get("error")
                    } else {
                        payload.get("content")
                    };
                    if let (Some(input), Some(output)) = (input, output) {
                        let call = (name, input, failed, output.clone());
                        run.tool_count = if run.last_tool.as_ref() == Some(&call) {
                            run.tool_count.saturating_add(1)
                        } else {
                            1
                        };
                        run.last_tool = Some(call);
                        run.triggered = run.tool_count >= 10;
                    } else {
                        run.last_tool = None;
                        run.tool_count = 0;
                    }
                }
            }
            _ => {}
        }
        run.triggered
    }
}
