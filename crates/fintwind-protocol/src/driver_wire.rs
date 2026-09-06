use anyhow::{Context as _, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::WireDriverEvent;
use crate::computer_use::{ComputerTarget, ComputerUsePhase, ComputerUseState};
use crate::model::{
    ActivityKind, DriverEvent, PermissionOption, ProviderRetryAction, UserInputQuestion,
};

pub fn decode_enum<T: DeserializeOwned>(value: &str) -> anyhow::Result<T> {
    serde_json::from_value(Value::String(value.to_owned()))
        .with_context(|| format!("invalid protocol enum value {value:?}"))
}

pub fn encode_enum<T: Serialize>(value: T) -> anyhow::Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("protocol enum did not serialize as a string"))
}

pub fn event_to_wire(event: DriverEvent) -> anyhow::Result<WireDriverEvent> {
    let (kind, payload) = match event {
        DriverEvent::RuntimeEventCursorAdvanced(_) => {
            bail!("client-only runtime cursors cannot be sent by the daemon")
        }
        DriverEvent::Connected { provider_cursor } => {
            ("connected", serde_json::to_value(provider_cursor)?)
        }
        DriverEvent::AgentPresetSelected(preset) => {
            ("agentPresetSelected", serde_json::to_value(preset)?)
        }
        DriverEvent::AutoTitleUpdated(title) => ("autoTitleUpdated", serde_json::to_value(title)?),
        DriverEvent::AvailableCommands(commands) => {
            ("availableCommands", serde_json::to_value(commands)?)
        }
        DriverEvent::TurnStarted => ("turnStarted", Value::Null),
        DriverEvent::TextDelta(text) => ("textDelta", Value::String(text)),
        DriverEvent::ReasoningDelta(text) => ("reasoningDelta", Value::String(text)),
        DriverEvent::Activity {
            id,
            kind,
            title,
            detail,
            complete,
        } => (
            "activity",
            json!({
                "id": id,
                "kind": kind,
                "title": title,
                "detail": detail,
                "complete": complete,
            }),
        ),
        DriverEvent::RichActivity(activity) => ("richActivity", serde_json::to_value(activity)?),
        DriverEvent::BackgroundWork(work) => ("backgroundWork", serde_json::to_value(work)?),
        DriverEvent::Permission {
            request_id,
            title,
            detail,
            options,
        } => (
            "permission",
            json!({
                "requestId": request_id,
                "title": title,
                "detail": detail,
                "options": options,
            }),
        ),
        DriverEvent::UserInputRequested {
            request_id,
            questions,
        } => (
            "userInputRequested",
            json!({
                "requestId": request_id,
                "questions": questions,
            }),
        ),
        DriverEvent::ComputerUseUpdated(state) => (
            "computerUseUpdated",
            serde_json::to_value(ComputerUseWire {
                target: state.target,
                phase: state.phase,
                visible: state.visible,
                image_url: state.image_url,
            })?,
        ),
        DriverEvent::SteerAccepted { message } => ("steerAccepted", json!({ "message": message })),
        DriverEvent::SteerRejected { message, reason } => (
            "steerRejected",
            json!({ "message": message, "reason": reason }),
        ),
        DriverEvent::UsageUpdated {
            context_tokens,
            context_window,
            session_total,
            cache_read,
            prompt_tokens,
        } => (
            "usageUpdated",
            json!({
                "contextTokens": context_tokens,
                "contextWindow": context_window,
                "sessionTotal": session_total,
                "cacheRead": cache_read,
                "promptTokens": prompt_tokens,
            }),
        ),
        DriverEvent::PlanUsageUpdated(usage) => ("planUsageUpdated", serde_json::to_value(usage)?),
        DriverEvent::CompactionUpdated(state) => {
            ("compactionUpdated", serde_json::to_value(state)?)
        }
        DriverEvent::ProviderBusy => ("providerBusy", Value::Null),
        DriverEvent::ProviderRetry {
            attempt,
            message,
            action,
            next_at_ms,
        } => (
            "providerRetry",
            json!({
                "attempt": attempt,
                "message": message,
                "action": action,
                "nextAtMs": next_at_ms,
            }),
        ),
        DriverEvent::TurnFinished { success, summary } => (
            "turnFinished",
            json!({ "success": success, "summary": summary }),
        ),
        DriverEvent::Error(error) => ("error", Value::String(error)),
        DriverEvent::NativeSessionsChanged => ("nativeSessionsChanged", Value::Null),
        DriverEvent::ProcessExited => ("processExited", Value::Null),
    };
    Ok(WireDriverEvent::new(kind, payload))
}

pub fn event_from_wire(event: WireDriverEvent) -> anyhow::Result<DriverEvent> {
    let payload = event.payload;
    Ok(match event.kind.as_str() {
        "connected" => DriverEvent::Connected {
            provider_cursor: serde_json::from_value(payload)?,
        },
        "agentPresetSelected" => DriverEvent::AgentPresetSelected(serde_json::from_value(payload)?),
        "autoTitleUpdated" => DriverEvent::AutoTitleUpdated(serde_json::from_value(payload)?),
        "availableCommands" => DriverEvent::AvailableCommands(serde_json::from_value(payload)?),
        "turnStarted" => DriverEvent::TurnStarted,
        "nativeSessionsChanged" => DriverEvent::NativeSessionsChanged,
        "textDelta" => DriverEvent::TextDelta(serde_json::from_value(payload)?),
        "reasoningDelta" => DriverEvent::ReasoningDelta(serde_json::from_value(payload)?),
        "activity" => {
            let activity: ActivityWire = serde_json::from_value(payload)?;
            DriverEvent::Activity {
                id: activity.id,
                kind: activity.kind,
                title: activity.title,
                detail: activity.detail,
                complete: activity.complete,
            }
        }
        "richActivity" => DriverEvent::RichActivity(serde_json::from_value(payload)?),
        "backgroundWork" => DriverEvent::BackgroundWork(serde_json::from_value(payload)?),
        "permission" => {
            let permission: PermissionWire = serde_json::from_value(payload)?;
            DriverEvent::Permission {
                request_id: permission.request_id,
                title: permission.title,
                detail: permission.detail,
                options: permission.options,
            }
        }
        "userInputRequested" => {
            let request: UserInputWire = serde_json::from_value(payload)?;
            DriverEvent::UserInputRequested {
                request_id: request.request_id,
                questions: request.questions,
            }
        }
        "computerUseUpdated" => {
            let state: ComputerUseWire = serde_json::from_value(payload)?;
            DriverEvent::ComputerUseUpdated(ComputerUseState {
                target: state.target,
                phase: state.phase,
                visible: state.visible,
                image_url: state.image_url,
            })
        }
        "steerAccepted" => {
            let steer: AcceptedSteerWire = serde_json::from_value(payload)?;
            DriverEvent::SteerAccepted {
                message: steer.message,
            }
        }
        "steerRejected" => {
            let steer: RejectedSteerWire = serde_json::from_value(payload)?;
            DriverEvent::SteerRejected {
                message: steer.message,
                reason: steer.reason,
            }
        }
        "usageUpdated" => {
            let usage: UsageWire = serde_json::from_value(payload)?;
            DriverEvent::UsageUpdated {
                context_tokens: usage.context_tokens,
                context_window: usage.context_window,
                session_total: usage.session_total,
                cache_read: usage.cache_read,
                prompt_tokens: usage.prompt_tokens,
            }
        }
        "planUsageUpdated" => DriverEvent::PlanUsageUpdated(serde_json::from_value(payload)?),
        "compactionUpdated" => {
            DriverEvent::CompactionUpdated(serde_json::from_value(payload)?)
        }
        "providerBusy" => DriverEvent::ProviderBusy,
        "providerRetry" => {
            let retry: ProviderRetryWire = serde_json::from_value(payload)?;
            DriverEvent::ProviderRetry {
                attempt: retry.attempt,
                message: retry.message,
                action: retry.action,
                next_at_ms: retry.next_at_ms,
            }
        }
        "turnFinished" => {
            let finished: TurnFinishedWire = serde_json::from_value(payload)?;
            DriverEvent::TurnFinished {
                success: finished.success,
                summary: finished.summary,
            }
        }
        "error" => DriverEvent::Error(serde_json::from_value(payload)?),
        "processExited" => DriverEvent::ProcessExited,
        kind => bail!("daemon sent an unsupported driver event {kind:?}"),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityWire {
    id: Option<String>,
    kind: ActivityKind,
    title: String,
    detail: Option<String>,
    complete: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PermissionWire {
    request_id: String,
    title: String,
    detail: String,
    options: Vec<PermissionOption>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInputWire {
    request_id: String,
    questions: Vec<UserInputQuestion>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ComputerUseWire {
    target: Option<ComputerTarget>,
    phase: ComputerUsePhase,
    visible: bool,
    image_url: Option<String>,
}

#[derive(Deserialize)]
struct AcceptedSteerWire {
    message: String,
}

#[derive(Deserialize)]
struct RejectedSteerWire {
    message: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageWire {
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    #[serde(default)]
    session_total: Option<u64>,
    #[serde(default)]
    cache_read: Option<u64>,
    #[serde(default)]
    prompt_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct TurnFinishedWire {
    success: bool,
    summary: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderRetryWire {
    attempt: u32,
    message: String,
    action: Option<ProviderRetryAction>,
    next_at_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ActivityItem, BackgroundWorkEvent, BackgroundWorkKey, BackgroundWorkKind,
        BackgroundWorkTranscript, BackgroundWorkTranscriptEvent, CompactionState,
        CompactionStatus, ReasoningBlock, UserInputOption, UserInputQuestion,
    };

    #[test]
    fn structured_user_input_round_trips_through_the_daemon_wire() {
        let wire = event_to_wire(DriverEvent::UserInputRequested {
            request_id: "request-1".into(),
            questions: vec![UserInputQuestion {
                id: "deployment".into(),
                header: "Environment".into(),
                question: "Where should this deploy?".into(),
                options: vec![UserInputOption {
                    label: "Preview".into(),
                    description: Some("Create a preview deployment".into()),
                }],
                multi_select: false,
            }],
        })
        .unwrap();
        assert_eq!(wire.kind, "userInputRequested");

        let DriverEvent::UserInputRequested {
            request_id,
            questions,
        } = event_from_wire(wire).unwrap()
        else {
            panic!("the event changed variants during its wire round trip");
        };
        assert_eq!(request_id, "request-1");
        assert_eq!(questions[0].id, "deployment");
        assert_eq!(questions[0].options[0].label, "Preview");
    }

    #[test]
    fn provider_status_signals_round_trip_through_the_daemon_wire() {
        let wire = event_to_wire(DriverEvent::ProviderBusy).unwrap();
        assert_eq!(wire.kind, "providerBusy");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ProviderBusy
        ));

        let expected_action = ProviderRetryAction {
            reason: "free_tier_limit".into(),
            provider: "zen".into(),
            title: "Free limit reached".into(),
            message: "Subscribe to OpenCode Go".into(),
            label: "subscribe".into(),
            link: Some("https://opencode.ai/go".into()),
        };
        let retry = DriverEvent::ProviderRetry {
            attempt: 3,
            message: "429 Too Many Requests".into(),
            action: Some(expected_action.clone()),
            next_at_ms: Some(1_700_000_008_000),
        };
        let wire = event_to_wire(retry).unwrap();
        assert_eq!(wire.kind, "providerRetry");
        assert!(matches!(
            event_from_wire(wire).unwrap(),
            DriverEvent::ProviderRetry {
                attempt: 3,
                ref message,
                action: Some(ref round_action),
                next_at_ms: Some(1_700_000_008_000),
            } if message == "429 Too Many Requests" && *round_action == expected_action
        ));
    }

    #[test]
    fn compaction_snapshots_round_trip_through_the_daemon_wire() {
        let snapshots = vec![
            CompactionState {
                status: CompactionStatus::Running,
                reason: Some("manual".into()),
                model: None,
                error: None,
            },
            CompactionState {
                status: CompactionStatus::Completed,
                reason: Some("auto".into()),
                model: Some("anthropic/claude-sonnet-4".into()),
                error: None,
            },
            CompactionState {
                status: CompactionStatus::Failed,
                reason: None,
                model: None,
                error: Some("provider rejected the summary call".into()),
            },
            CompactionState {
                status: CompactionStatus::Cancelled,
                reason: Some("manual".into()),
                model: None,
                error: None,
            },
        ];
        for state in snapshots {
            let wire = event_to_wire(DriverEvent::CompactionUpdated(state.clone())).unwrap();
            assert_eq!(wire.kind, "compactionUpdated");
            let DriverEvent::CompactionUpdated(round_tripped) =
                event_from_wire(wire).unwrap()
            else {
                panic!("the event changed variants during its wire round trip");
            };
            assert_eq!(round_tripped, state);
        }
    }

    #[test]
    fn background_work_transcript_events_round_trip_through_the_daemon_wire() {
        let key = BackgroundWorkKey {
            kind: BackgroundWorkKind::Subagent,
            provider_id: "ses_child".into(),
        };
        let activity = ActivityItem::from_reasoning(
            ReasoningBlock {
                content: "thinking".into(),
                started_at_ms: 1,
                finished_at_ms: 2,
            },
            true,
        );
        // Every variant must survive both serde tag layers: the inner
        // externally tagged transcript variant and the outer internally
        // tagged backgroundWork payload.
        let events = vec![
            BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Started {
                key: key.clone(),
                prompt: Some("Inspect the repository".into()),
            }),
            BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::ReasoningDelta {
                key: key.clone(),
                delta: "thinking".into(),
            }),
            BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::TextDelta {
                key: key.clone(),
                delta: "answer".into(),
            }),
            BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Activity {
                key: key.clone(),
                activity: activity.clone(),
            }),
            BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Snapshot {
                key: key.clone(),
                transcript: BackgroundWorkTranscript::default(),
            }),
            BackgroundWorkEvent::Transcript(BackgroundWorkTranscriptEvent::Finished {
                key: key.clone(),
                success: true,
            }),
        ];
        for event in events {
            let wire = event_to_wire(DriverEvent::BackgroundWork(event.clone())).unwrap();
            assert_eq!(wire.kind, "backgroundWork");
            let DriverEvent::BackgroundWork(round_tripped) = event_from_wire(wire).unwrap() else {
                panic!("the event changed variants during its wire round trip");
            };
            // Serialized-shape equality doubles as a wire-contract check:
            // both sides must tag the transcript variant identically.
            assert_eq!(
                serde_json::to_value(&round_tripped).unwrap(),
                serde_json::to_value(&event).unwrap()
            );
        }
    }
}
