//! Bounded transport diagnostics. Only local identifiers and fixed reason
//! codes belong here; never prompts, event payloads or authentication headers.

use std::io::Write as _;

pub(crate) fn submission(
    reason: &str,
    record: &fintwind_protocol::submission::SubmissionRecord,
    port: u16,
    generation: u64,
) {
    // JSON escaping keeps provider IDs from injecting extra log lines. Never
    // serialize the record itself: it contains the private prompt and files.
    let line = serde_json::json!({
        "time": crate::model::unix_time_millis(), "reason": reason,
        "session_id": record.session_id, "submission_id": record.receipt.id,
        "input_id": record.receipt.input_id, "state": record.receipt.state,
        "native_session_id": record.receipt.native_session_id,
        "port": port, "generation": generation,
    })
    .to_string();
    append(&line);
}

pub(crate) fn record(reason: &str, port: u16, generation: u64) {
    eprintln!("OpenCode sync: {reason}, port={port}, generation={generation}");
    append(&format!(
        "time={} reason={reason} port={port} generation={generation}",
        crate::model::unix_time_millis()
    ));
}

fn append(line: &str) {
    // Called only from background workers, never rendering.
    let Some(directory) = crate::persistence::StateStore::default_path()
        .parent()
        .map(std::path::Path::to_path_buf)
    else {
        return;
    };
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("opencode-sync.log");
        // Once full, keep the initial evidence rather than unbounded logging.
        // The file intentionally contains no conversation content.
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= 2 * 1024 * 1024) {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(file, "{line}")
    };
    let _ = write();
}
