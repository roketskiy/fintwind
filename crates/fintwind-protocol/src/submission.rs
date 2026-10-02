//! Durable admission facts, separate from provider execution/completion.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{PromptFile, model::unix_time_millis};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SubmissionState {
    Preparing,
    Dispatching,
    Accepted,
    Unknown,
    NotSent,
    Rejected,
}

impl SubmissionState {
    pub fn is_unconfirmed(self) -> bool {
        matches!(self, Self::Dispatching | Self::Unknown)
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Self::Preparing => "submission.preparing",
            Self::Dispatching => "submission.dispatching",
            Self::Accepted => "submission.accepted",
            Self::Unknown => "submission.unknown",
            Self::NotSent => "submission.not_sent",
            Self::Rejected => "submission.rejected",
        }
    }
}

impl SubmissionReceipt {
    pub fn supersedes(&self, previous: &Self) -> bool {
        if self.id != previous.id || self.input_id != previous.input_id {
            return false;
        }
        let rank = |state| match state {
            SubmissionState::Preparing => 0,
            SubmissionState::Dispatching => 1,
            SubmissionState::Unknown => 2,
            _ => 3,
        };
        let next = rank(self.state);
        let prior = rank(previous.state);
        next > prior
            || (next == prior
                && self.updated_at >= previous.updated_at
                && (self.native_session_id.is_some() || previous.native_session_id.is_none()))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmissionReceipt {
    /// Also the local turn ID. Never regenerated for an RPC retry.
    pub id: Uuid,
    pub input_id: String,
    pub state: SubmissionState,
    pub created_at: u64,
    pub updated_at: u64,
    pub native_session_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SubmissionRecord {
    pub session_id: Uuid,
    pub receipt: SubmissionReceipt,
    /// Frozen provider-facing text, including template expansion.
    pub prompt: String,
    pub files: Vec<PromptFile>,
}

impl SubmissionRecord {
    pub fn new(session_id: Uuid, turn_id: Uuid, prompt: String, files: Vec<PromptFile>) -> Self {
        let created_at = unix_time_millis();
        Self {
            session_id,
            receipt: SubmissionReceipt {
                id: turn_id,
                input_id: new_input_id(created_at),
                state: SubmissionState::Preparing,
                created_at,
                updated_at: created_at,
                native_session_id: None,
            },
            prompt,
            files,
        }
    }
}

/// OpenCode's sortable message-ID shape. The ID is persisted before any POST.
pub fn new_input_id(timestamp: u64) -> String {
    let prefix = timestamp.wrapping_mul(0x1000) & 0xffff_ffff_ffff;
    let suffix = Uuid::new_v4().simple().to_string();
    format!("msg_{prefix:012x}{}", &suffix[..14])
}

/// Keep local admission identities while allowing new native history to grow.
/// Run this on a background worker, never in a per-frame row builder.
pub fn merge_native_transcript(
    local: &crate::model::AgentSession,
    mut native: crate::provider_session::NativeTranscript,
) -> crate::provider_session::NativeTranscript {
    use crate::model::MessageRole;
    use std::collections::{HashMap, HashSet};
    let owned = local
        .turns
        .iter()
        .filter_map(|turn| {
            turn.submission
                .as_ref()
                .map(|receipt| (receipt.input_id.as_str(), turn))
        })
        .collect::<HashMap<_, _>>();
    let mut remapped = HashMap::new();
    let mut seen = HashSet::new();
    for turn in &mut native.turns {
        let Some(saved) = turn
            .provider_resume_at
            .as_deref()
            .and_then(|input| owned.get(input))
        else {
            continue;
        };
        seen.insert(saved.id);
        remapped.insert(turn.id, saved.id);
        turn.id = saved.id;
        turn.submission = saved.submission.clone();
        turn.checkpoint = saved.checkpoint.clone();
        turn.provider_prompt = saved.provider_prompt.clone();
        // The history converter's completed model step is not an execution
        // terminal. Preserve the local status until receipt-aware proof arrives.
        turn.status = saved.status;
        turn.completed_at = saved.completed_at;
    }
    for message in &mut native.messages {
        if let Some(id) = message.turn_id.and_then(|id| remapped.get(&id).copied()) {
            message.turn_id = Some(id);
            if message.role == MessageRole::User
                && let Some(saved) = local
                    .messages
                    .iter()
                    .find(|saved| saved.turn_id == Some(id) && saved.role == MessageRole::User)
            {
                // Keep the typed command, display presentation and attachments,
                // not the expanded transport text the provider stored.
                *message = saved.clone();
            }
        }
    }
    for block in &mut native.blocks {
        if let Some(id) = block.turn_id.and_then(|id| remapped.get(&id).copied()) {
            block.turn_id = Some(id);
        }
    }
    for turn in local
        .turns
        .iter()
        .filter(|turn| turn.submission.is_some() && !seen.contains(&turn.id))
    {
        let next_turn = native
            .turns
            .iter()
            .find(|next| next.started_at > turn.started_at)
            .map(|next| next.id);
        let insertion = next_turn
            .and_then(|id| {
                native
                    .messages
                    .iter()
                    .position(|message| message.turn_id == Some(id))
            })
            .unwrap_or(native.messages.len());
        let messages = local
            .messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.turn_id == Some(turn.id))
            .collect::<Vec<_>>();
        for block in &mut native.blocks {
            if block.after_message >= insertion {
                block.after_message += messages.len();
            }
        }
        native.blocks.extend(
            local
                .transcript_blocks
                .iter()
                .filter(|block| block.turn_id == Some(turn.id))
                .map(|block| {
                    let mut block = block.clone();
                    block.after_message = insertion
                        + messages
                            .iter()
                            .filter(|(index, _)| *index < block.after_message)
                            .count();
                    block
                }),
        );
        native.messages.splice(
            insertion..insertion,
            messages.iter().map(|(_, message)| (*message).clone()),
        );
        let turn_insertion = next_turn
            .and_then(|id| native.turns.iter().position(|next| next.id == id))
            .unwrap_or(native.turns.len());
        native.turns.insert(turn_insertion, turn.clone());
    }
    for (index, turn) in native.turns.iter_mut().enumerate() {
        turn.turn_count = index + 1;
    }
    native.blocks.sort_by_key(|block| block.after_message);
    native
}

/// A newer external turn cannot release an unresolved locally owned input.
/// Used at admission/reconciliation boundaries, not in per-row rendering.
pub fn has_unsettled_submissions(session: &crate::model::AgentSession) -> bool {
    session.turns.iter().any(|turn| {
        turn.submission.as_ref().is_some_and(|receipt| {
            turn.status == crate::model::TurnStatus::Running
                || receipt.state == SubmissionState::Preparing
                || receipt.state.is_unconfirmed()
        })
    })
}

/// Replace the suffix starting at an exact input with its verified newer window.
/// Prefix rows and their ordered blocks come from the full history read.
pub fn replace_native_window(
    full: &mut crate::provider_session::NativeTranscript,
    window: crate::provider_session::NativeTranscript,
    input_id: &str,
) -> bool {
    if !window
        .turns
        .first()
        .is_some_and(|turn| turn.provider_resume_at.as_deref() == Some(input_id))
    {
        return false;
    }
    let Some(turn_index) = full
        .turns
        .iter()
        .position(|turn| turn.provider_resume_at.as_deref() == Some(input_id))
    else {
        return false;
    };
    let turn_id = full.turns[turn_index].id;
    let Some(message_index) = full
        .messages
        .iter()
        .position(|message| message.turn_id == Some(turn_id))
    else {
        return false;
    };
    let tail_ids = full.turns[turn_index..]
        .iter()
        .map(|turn| turn.id)
        .collect::<std::collections::HashSet<_>>();
    full.messages.truncate(message_index);
    full.turns.truncate(turn_index);
    full.blocks.retain(|block| {
        block.after_message <= message_index
            && block.turn_id.is_none_or(|id| !tail_ids.contains(&id))
    });
    full.messages.extend(window.messages);
    full.turns.extend(window.turns);
    full.blocks
        .extend(window.blocks.into_iter().map(|mut block| {
            block.after_message += message_index;
            block
        }));
    true
}
