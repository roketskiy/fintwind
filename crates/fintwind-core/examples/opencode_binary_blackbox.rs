//! Settings persistence and provider discovery through their public module APIs.
//! No executables are launched and no user configuration is written.

use anyhow::{Context, Result, ensure};
use fintwind_client::persistence::PersistedState;
use fintwind_core::{DaemonSettingsStore, model};
use fintwind_protocol::settings::OPENCODE_BINARY_PATH_KEY;
use serde_json::json;
use std::path::PathBuf;
use uuid::Uuid;

fn main() -> Result<()> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("pass an artifact directory")?,
    )
    .join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    let settings_path = output.join("settings.json");
    let store = DaemonSettingsStore::open(settings_path.clone())?;
    let automatic = model::provider_probe(&store.get(), None).path;
    let portable = output.join("便携 OpenCode").join("opencode-cli.exe");
    std::fs::create_dir_all(portable.parent().unwrap())?;
    // Discovery never executes these files; they deliberately contain no code.
    std::fs::write(&portable, b"discovery fixture, not an executable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&portable, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut checks = Vec::new();
    let mut client = PersistedState::empty();
    client.apply_daemon_settings(store.get());
    client.set_opencode_binary_path(Some(
        portable.to_str().context("UTF-8 fixture path")?.into(),
    ));
    let mut settings = client.daemon_settings();
    settings.extra.insert("future_setting".into(), json!(42));
    store.replace(settings)?;
    ensure!(model::provider_probe(&store.get(), None).path.as_ref() == Some(&portable));
    checks.push("portable filename with Chinese and spaces resolves without PATH");

    let store = DaemonSettingsStore::open(settings_path.clone())?;
    client.apply_daemon_settings(store.get());
    ensure!(client.opencode_binary_path() == portable.to_str());
    ensure!(model::provider_probe(&store.get(), None).path.as_ref() == Some(&portable));
    ensure!(store.get().extra["future_setting"] == 42);
    checks.push("reopening settings restores the path and preserves unrelated values");

    let before = std::fs::read(&settings_path)?;
    for invalid in [
        output.join("missing.exe"),
        output.clone(),
        PathBuf::from("relative-opencode.exe"),
    ] {
        let mut rejected = store.get();
        rejected
            .extra
            .insert(OPENCODE_BINARY_PATH_KEY.into(), json!(invalid));
        ensure!(
            store.replace(rejected).is_err(),
            "accepted {}",
            invalid.display()
        );
        ensure!(std::fs::read(&settings_path)? == before);
        ensure!(model::provider_probe(&store.get(), None).path.as_ref() == Some(&portable));
    }
    #[cfg(windows)]
    {
        let text = output.join("opencode.txt");
        std::fs::write(&text, b"not executable")?;
        let mut rejected = store.get();
        rejected
            .extra
            .insert(OPENCODE_BINARY_PATH_KEY.into(), json!(text));
        ensure!(store.replace(rejected).is_err());
        ensure!(std::fs::read(&settings_path)? == before);
    }
    checks.push("invalid paths leave both persisted and active settings intact");

    let missing_override = output.join("explicit-missing.exe");
    ensure!(!model::provider_probe(&store.get(), missing_override.to_str()).installed);
    checks.push("an explicit request override takes precedence over saved configuration");

    std::fs::remove_file(&portable)?;
    ensure!(!model::provider_probe(&store.get(), None).installed);
    let mut settings = store.get();
    settings.extra.insert("future_setting".into(), json!(43));
    store.replace(settings)?;
    checks.push("a removed executable does not silently fall back or block unrelated settings");

    client.apply_daemon_settings(store.get());
    client.set_opencode_binary_path(None);
    store.replace(client.daemon_settings())?;
    let store = DaemonSettingsStore::open(settings_path)?;
    ensure!(store.get().opencode_binary_path().is_none());
    ensure!(model::provider_probe(&store.get(), None).path == automatic);
    ensure!(store.get().extra["future_setting"] == 43);
    checks.push("reset persists and restores the original automatic discovery result");
    std::fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&json!({
            "passed": true, "checks": checks, "processes_launched": 0
        }))?,
    )?;
    println!("{} checks passed; {}", checks.len(), output.display());
    Ok(())
}
