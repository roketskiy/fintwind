//! Transient session generation, independent of the durable provider runner.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::bail;
use parking_lot::Mutex;
use serde_json::json;
use uuid::Uuid;

const MAX_GENERATIONS: usize = 64;
const GENERATION_TIMEOUT: Duration = Duration::from_secs(120);
const INSTRUCTIONS: &str = "The user is asking a quick side question about the conversation so far. Answer directly and concisely in markdown from what you already know. Do not call any tools and do not take any actions.";

#[derive(Default)]
struct Requests {
    active: HashMap<Uuid, Arc<AtomicBool>>,
    // A cancel can overtake registration on the daemon's request threads.
    // Bounded tombstones also make a late cancel after completion harmless.
    cancelled: VecDeque<Uuid>,
}

#[derive(Default)]
pub(crate) struct SessionGenerations(Mutex<Requests>);

impl SessionGenerations {
    pub(crate) fn cancel(&self, id: Uuid) {
        let mut requests = self.0.lock();
        if let Some(cancelled) = requests.active.get(&id) {
            cancelled.store(true, Ordering::Release);
        } else if !requests.cancelled.contains(&id) {
            if requests.cancelled.len() == MAX_GENERATIONS {
                requests.cancelled.pop_front();
            }
            requests.cancelled.push_back(id);
        }
    }

    pub(crate) fn generate(
        &self,
        id: Uuid,
        binary: &Path,
        directory: &Path,
        session_id: &str,
        question: &str,
        model: Option<&fintwind_protocol::provider_session::SessionGenerationModel>,
    ) -> anyhow::Result<Option<String>> {
        let question = question.trim();
        if question.is_empty() {
            bail!(tr!("btw.question_required"));
        }
        let cancelled = {
            let mut requests = self.0.lock();
            if requests.cancelled.contains(&id) {
                bail!(tr!("btw.cancelled"));
            }
            if requests.active.contains_key(&id) || requests.active.len() >= MAX_GENERATIONS {
                bail!("side generation is already running or at capacity");
            }
            let cancelled = Arc::new(AtomicBool::new(false));
            requests.active.insert(id, cancelled.clone());
            cancelled
        };
        let result = (|| {
            let server = crate::opencode_pool::acquire(binary, directory)?;
            let session_path = format!(
                "/api/session/{}",
                crate::opencode_session::encode_path_segment(session_id)
            );
            if let Some(expected) = model {
                // Model picks normally take effect on the next driver start.
                // Do not silently generate with the previous selection, or
                // mutate the shared session while another client is working.
                let snapshot = crate::opencode_session::request_json_on_port_cancellable(
                    server.port,
                    "GET",
                    &session_path,
                    None,
                    Duration::from_secs(10),
                    &cancelled,
                )?;
                let native = snapshot.get("data").unwrap_or(&snapshot).get("model");
                let matches = expected
                    .model
                    .split_once('/')
                    .is_some_and(|(provider, id)| {
                        native.is_some_and(|native| {
                            native.get("providerID").and_then(serde_json::Value::as_str)
                                == Some(provider)
                                && native.get("id").and_then(serde_json::Value::as_str) == Some(id)
                                && native.get("variant").and_then(serde_json::Value::as_str)
                                    == expected.variant.as_deref()
                        })
                    });
                if !matches {
                    return Ok(None);
                }
            }
            let response = crate::opencode_session::request_json_on_port_cancellable(
                server.port,
                "POST",
                &format!("{session_path}/generate"),
                Some(&json!({"prompt": format!("{INSTRUCTIONS}\n\n{question}")})),
                GENERATION_TIMEOUT,
                &cancelled,
            )?;
            let text = response
                .get("data")
                .and_then(|data| data.get("text"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!(tr!("btw.invalid_response")))?
                .trim();
            if text.is_empty() {
                bail!(tr!("btw.empty_answer"));
            }
            Ok(Some(text.to_owned()))
        })();
        self.0.lock().active.remove(&id);
        result
    }
}
