//! Synthetic cross-language stream producer. No sockets, user data, or services.
#[allow(dead_code)]
#[path = "../src/crypto.rs"]
mod crypto;
#[allow(dead_code)]
#[path = "../src/snapshot.rs"]
mod snapshot;

use serde_json::{Value, json};
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let watch_id = "10000000-0000-4000-8000-000000000001";
    let session_id = "10000000-0000-4000-8000-000000000002";
    let mut entries: Vec<Value> = (0..120)
        .map(|i| {
            json!({
                "type":"message", "id":format!("row-{i}"),
                "role": if i % 2 == 0 { "user" } else { "assistant" },
                "text": "History text. ".repeat(64), "createdAt":1793635200,
                "elapsedSeconds":null, "attachments":[], "streaming":false,
            })
        })
        .collect();
    entries.push(json!({"type":"activity", "id":"reasoning", "kind":"reasoning", "title":"", "text":"", "complete":false, "failed":false, "truncated":false}));
    entries.push(json!({"type":"message", "id":"live", "role":"assistant", "text":"", "attachments":[], "streaming":true}));
    let mut value = json!({"id": session_id, "projectId":"synthetic", "title":"Synthetic stream", "status":"working",
        "messages":[], "entries":entries, "features":["orderedTranscript","sessionDeltas","sessionPermissions"],
        "model":"test", "modelName":"Test", "interactionMode":"build", "reasoningEffort":"high", "runtimeMode":"ask",
        "permission":null, "questions":null, "desktopRequired":false, "canSend":true, "truncated":false, "goal":null});
    let (mut outbound, _) = crypto::directions(&[7; 32], &[11; 32], &[17; 32])?;
    let mut previous: Option<Value> = None;
    let mut frames = Vec::new();
    let mut expected = Vec::new();
    let mut full_bytes = 0;
    let mut update_bytes = 0;
    let start = Instant::now();
    for tick in 0..60 {
        value["usage"] = json!({"contextTokens": 42_000 + tick, "contextWindow": 200_000,
            "totalTokens": 1_234_567 + tick, "cacheReadTokens": 800_000, "promptTokens": 1_000_000});
        if tick == 0 { value["usage"]["contextTokens"] = Value::Null; }
        if tick == 20 { value["usage"]["cacheReadTokens"] = json!(0); value["usage"]["promptTokens"] = json!(0); }
        if tick == 30 { value["usage"]["contextWindow"] = Value::Null; }
        if tick == 40 { value["usage"]["totalTokens"] = Value::Null; }
        if tick == 59 { value["usage"] = Value::Null; }
        if tick == 15 { value["runtimeMode"] = json!("autoAcceptEdits"); }
        if tick == 35 { value["runtimeMode"] = json!("fullAccess"); }
        if tick == 55 { value["runtimeMode"] = json!("ask"); }
        let list = value["entries"].as_array_mut().unwrap();
        let live = list.iter_mut().find(|row| row["id"] == "live").unwrap();
        live["text"] = json!(format!(
            "{}Line {tick}: unicode \u{4e2d}\u{6587} \u{1f642}\n",
            live["text"].as_str().unwrap()
        ));
        if tick == 20 {
            list.iter_mut()
                .find(|row| row["id"] == "reasoning")
                .unwrap()["complete"] = json!(true);
        }
        if tick == 30 {
            list.remove(0);
        }
        if tick == 40 {
            list.push(json!({"type":"activity", "id":"tool", "kind":"tool", "title":"Read", "text":"", "complete":true, "failed":false, "truncated":false}));
        }
        if tick == 50 {
            value["permission"] = json!({"requestId":"request", "title":"Synthetic approval", "options":[{"id":"allow", "label":"Allow", "allow":true}]});
        }
        if tick == 59 {
            value["status"] = json!("idle");
            value["entries"].as_array_mut().unwrap().pop();
        }
        let next = snapshot::conditional_response(value.clone(), None);
        let frame = match &previous {
            None => json!({"type":"sessionUpdate", "watchId":watch_id, "snapshot":next}),
            Some(old) => {
                json!({"type":"sessionUpdate", "watchId":watch_id, "baseRevision":old["revision"], "revision":next["revision"], "edits":snapshot::delta(old, &next)})
            }
        };
        let bytes = serde_json::to_vec(&frame)?;
        update_bytes += bytes.len();
        full_bytes += serde_json::to_vec(&next)?.len();
        let (seq, encrypted) = outbound.seal(&bytes)?;
        frames.push(json!({"type":"sealed", "seq":seq, "data":crypto::encode(&encrypted)}));
        expected.push(next.clone());
        previous = Some(next);
    }
    println!(
        "{}",
        json!({"sessionId":session_id, "watchId":watch_id, "frames":frames, "expected":expected,
        "metrics":{"frames":frames.len(), "fullSnapshotBytes":full_bytes, "updateBytes":update_bytes,
            "producerMilliseconds": start.elapsed().as_secs_f64()*1000.0}})
    );
    Ok(())
}
