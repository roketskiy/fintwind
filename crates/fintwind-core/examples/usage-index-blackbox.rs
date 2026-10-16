//! Repeatable integration scenario; no application/provider processes launched.
use fintwind_core::usage_index::{UsageIndex, UsageSource};
use fintwind_protocol::provider_session::UsageStats;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Provider {
    rows: Mutex<Vec<Value>>,
    messages: Mutex<Vec<Value>>,
    calls: AtomicUsize,
    fail: Mutex<bool>,
    cyclic_listing: Mutex<bool>,
}
impl UsageSource for Provider {
    fn request(&self, path: &str) -> anyhow::Result<Value> {
        anyhow::ensure!(!*self.fail.lock().unwrap(), "controlled provider failure");
        if path.starts_with("/api/session?") {
            let rows = self.rows.lock().unwrap();
            let start = path
                .split("cursor=")
                .nth(1)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let end = (start + 500).min(rows.len());
            return Ok(json!({"data": rows[start..end], "cursor":{"next":
                if *self.cyclic_listing.lock().unwrap() { Some("0".to_owned()) }
                else if end < rows.len() { Some(end.to_string()) } else { None }}}));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        if path.contains("/unavailable/") {
            return Ok(json!({"data":[]}));
        }
        anyhow::ensure!(
            !path.contains("/broken/"),
            "controlled message read failure"
        );
        if path.contains("/child/") {
            return Ok(json!({"data":[message("child-1","C",5)]}));
        }
        Ok(json!({"data": self.messages.lock().unwrap().clone()}))
    }
}
fn read(index: &UsageIndex, source: &str, provider: &Arc<Provider>) -> UsageStats {
    let provider = provider.clone();
    index
        .snapshot(source.into(), false, move || {
            Ok(provider as Arc<dyn UsageSource>)
        })
        .unwrap()
}
fn settled(index: &UsageIndex, source: &str, provider: &Arc<Provider>) -> UsageStats {
    let start = Instant::now();
    loop {
        let stats = read(index, source, provider);
        if !stats.refreshing {
            return stats;
        }
        assert!(start.elapsed() < Duration::from_secs(120));
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn message(id: &str, model: &str, input: u64) -> Value {
    json!({"id": id, "type":"assistant", "time":{"created":1700000000000u64},
        "model":{"providerID":"p","id":model},"tokens":{"input":input}})
}
fn main() -> anyhow::Result<()> {
    let directory = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .expect("pass an exclusive artifact directory");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("usage-{}.db", uuid::Uuid::new_v4()));
    let provider = Arc::new(Provider {
        rows: Mutex::new(vec![
            json!({"id":"s","time":{"created":1700000000000u64,"updated":1700000010000u64},
            "model":{"providerID":"p","id":"B"},"tokens":{"input":60}}),
        ]),
        messages: Mutex::new(vec![
            message("1", "A", 10),
            message("2", "B", 20),
            message("3", "A", 30),
        ]),
        calls: AtomicUsize::new(0),
        fail: Mutex::new(false),
        cyclic_listing: Mutex::new(false),
    });
    let index = UsageIndex::new(path.clone());
    for _ in 0..20 {
        read(&index, "one", &provider);
    }
    let first = settled(&index, "one", &provider);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1, "single-flight");
    let lanes = &first.entries[0].model_lanes;
    assert_eq!(
        lanes.iter().find(|lane| lane.model == "p/A").unwrap().total,
        40
    );
    assert_eq!(
        lanes.iter().find(|lane| lane.model == "p/B").unwrap().total,
        20
    );
    let restart = UsageIndex::new(path.clone());
    let start = Instant::now();
    let cached = read(&restart, "one", &provider);
    let snapshot_ms = start.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(cached.entries, first.entries, "restart snapshot");
    settled(&restart, "one", &provider);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "unchanged history not read"
    );

    // Amended facts replace the old session walk rather than adding it twice.
    provider.rows.lock().unwrap()[0]["tokens"]["input"] = json!(70);
    provider.messages.lock().unwrap()[0]["tokens"]["input"] = json!(20);
    let amended = UsageIndex::new(path.clone());
    let updated = settled(&amended, "one", &provider);
    assert_eq!(updated.entries[0].total_tokens(), 70);
    assert_eq!(
        updated.entries[0]
            .model_lanes
            .iter()
            .map(|lane| lane.total)
            .sum::<u64>(),
        70
    );

    *provider.cyclic_listing.lock().unwrap() = true;
    let incomplete = UsageIndex::new(path.clone());
    let retained = settled(&incomplete, "one", &provider);
    assert!(retained.refresh_error.is_some());
    assert_eq!(
        retained.entries, updated.entries,
        "cyclic listing does not erase history"
    );
    *provider.cyclic_listing.lock().unwrap() = false;

    provider
        .rows
        .lock()
        .unwrap()
        .push(json!({"id":"child","parentID":"s",
        "time":{"created":1700000000000u64,"updated":1700000010000u64},"tokens":{"input":5}}));
    let children = UsageIndex::new(path.clone());
    let folded = settled(&children, "one", &provider);
    assert_eq!(folded.entries.len(), 1);
    assert_eq!(folded.entries[0].total_tokens(), 75);
    assert_eq!(
        folded.entries[0].subagent_models[0].model.as_deref(),
        Some("p/C")
    );
    provider.rows.lock().unwrap().pop();

    *provider.fail.lock().unwrap() = true;
    let failed = UsageIndex::new(path.clone());
    let retained = settled(&failed, "one", &provider);
    assert_eq!(
        retained.entries, folded.entries,
        "failure retains verified data"
    );
    assert!(retained.refresh_error.is_some());
    *provider.fail.lock().unwrap() = false;
    provider.messages.lock().unwrap()[0]
        .as_object_mut()
        .unwrap()
        .remove("model");
    let other = settled(&index, "two", &provider);
    assert_eq!(
        other.entries[0].model, None,
        "unknown never inherits session model"
    );
    assert_eq!(
        other.entries[0]
            .model_lanes
            .iter()
            .map(|lane| lane.total)
            .sum::<u64>(),
        50
    );
    assert_eq!(
        read(&index, "one", &provider).entries,
        first.entries,
        "source isolation"
    );

    for id in ["unavailable", "broken"] {
        provider.rows.lock().unwrap().push(json!({"id":id,
            "time":{"created":1700000000000u64,"updated":1700000010000u64},"tokens":{"input":10}}));
    }
    let partial = settled(&index, "partial", &provider);
    assert!(
        partial.refresh_error.is_none(),
        "individual failures stay out of the page-level error"
    );
    assert_eq!(partial.entries.len(), 3);
    assert_eq!(
        partial
            .entries
            .iter()
            .map(|entry| entry.total_tokens())
            .sum::<u64>(),
        90
    );
    assert!(
        partial
            .entries
            .iter()
            .any(|entry| entry.model_lanes.iter().any(|lane| lane.model == "p/B")),
        "readable session remains attributed"
    );
    provider.rows.lock().unwrap().clear();
    let deleted = UsageIndex::new(path.clone());
    assert!(settled(&deleted, "two", &provider).entries.is_empty());

    let large_rows: Vec<_> = (0..1000)
        .map(|id| {
            json!({"id":format!("large-{id}"),
        "time":{"created":1700000000000u64,"updated":1700000010000u64},"tokens":{"input":70}})
        })
        .collect();
    *provider.rows.lock().unwrap() = large_rows;
    let started = Instant::now();
    let large = UsageIndex::new(path.clone());
    let indexed = settled(&large, "large", &provider);
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(indexed.entries.len(), 1000);
    assert_eq!(
        indexed
            .entries
            .iter()
            .map(|entry| entry.total_tokens())
            .sum::<u64>(),
        70000
    );
    let before = provider.calls.load(Ordering::SeqCst);
    let large_restart = UsageIndex::new(path.clone());
    let started = Instant::now();
    assert_eq!(read(&large_restart, "large", &provider).entries.len(), 1000);
    let large_snapshot_ms = started.elapsed().as_secs_f64() * 1000.0;
    settled(&large_restart, "large", &provider);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        before,
        "large unchanged history not read"
    );
    let artifact = json!({"passed":true,"database":path,"snapshotMs":snapshot_ms,
        "largeSessions":1000,"largeSnapshotMs":large_snapshot_ms,"largeBuildMs":build_ms,
        "messageRequests":provider.calls.load(Ordering::SeqCst),
        "cases":["A-B-A attribution","single-flight","restart cache","unchanged history",
        "amendment replacement","failure retention","unknown model","source isolation","deletion",
        "cyclic listing retention","subagent attribution","1000-session restart",
        "unsynchronized session skipped silently","message failure skipped silently"]});
    std::fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&artifact)?,
    )?;
    println!("{artifact}");
    Ok(())
}
