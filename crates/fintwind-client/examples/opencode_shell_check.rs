//! Black-box config round trip. No service, child processes, or user config writes.
//! Failure cases: absent config, invalid JSON, non-object root, escaped paths,
//! lost comments/unknown fields, duplicate shell keys, reset, stale external edits.
use fintwind_client::opencode_shell;
use std::{fs, io};

fn main() -> io::Result<()> {
    let directory = std::env::args_os()
        .nth(1)
        .expect("provide a new artifact directory");
    let directory = std::path::PathBuf::from(directory);
    fs::create_dir(&directory)?;
    let path = directory.join("opencode.jsonc");
    assert_eq!(opencode_shell::load_at(&path)?, None);
    let shell = r#"C:\Program Files\PowerShell\7\pwsh.exe"#;
    opencode_shell::save_at(&path, Some(shell))?;
    assert_eq!(opencode_shell::load_at(&path)?.as_deref(), Some(shell));
    let source = "{\n  // keep this comment\n  \"unknown\": {\"nested\": true},\n  \"shell\": \"old\",\n  \"shell\": \"duplicate\",\n}\n";
    fs::write(&path, source)?;
    opencode_shell::save_at(&path, Some(shell))?;
    let saved = fs::read_to_string(&path)?;
    assert!(saved.contains("// keep this comment"));
    assert!(saved.contains("\"unknown\": {\"nested\": true}"));
    assert_eq!(opencode_shell::load_at(&path)?.as_deref(), Some(shell));
    fs::write(&path, saved.replace("true", "false"))?;
    opencode_shell::save_at(&path, None)?;
    assert_eq!(opencode_shell::load_at(&path)?, None);
    assert!(fs::read_to_string(&path)?.contains("\"nested\": false"));
    fs::copy(&path, directory.join("round-trip.jsonc"))?;
    for invalid in ["{broken", "[]"] {
        fs::write(&path, invalid)?;
        assert!(opencode_shell::load_at(&path).is_err());
        assert!(opencode_shell::save_at(&path, Some(shell)).is_err());
        assert_eq!(fs::read_to_string(&path)?, invalid);
    }
    let detected = opencode_shell::detect();
    for option in &detected {
        assert!(std::path::Path::new(&option.path).is_file());
    }
    let report = serde_json::json!({"result": "pass", "checks": ["missing", "round-trip", "comments", "unknown-fields", "duplicates", "external-edit", "reset", "invalid-config", "discovery"], "detected": detected.iter().map(|option| serde_json::json!({"label": option.label, "path": option.path})).collect::<Vec<_>>()});
    fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("[OK] {}", directory.join("report.json").display());
    Ok(())
}
