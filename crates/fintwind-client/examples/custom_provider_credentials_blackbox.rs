//! One isolated black-box check. Run through scripts/provider-credentials-check.py.
use std::{io, path::PathBuf};

use anyhow::{Result, ensure};
use fintwind_client::{DaemonSupervisor, opencode_config as config, persistence::StateStore};
use serde_json::{Value, json};

fn check(args: &[String]) -> Result<Value> {
    let root = PathBuf::from(&args[1]);
    let binary = PathBuf::from(&args[3]);
    let path = config::config_path();
    let original = json!({
        "$schema": "https://opencode.ai/config.json",
        "provider": {
            "fixture": {"npm": "@ai-sdk/openai-compatible", "name": "Fixture",
                "options": {"apiKey": "fixture-old", "baseURL": args[4], "setCacheKey": true},
                "models": {"fixture-model": {"name": "Fixture", "tool_call": true,
                    "limit": {"context": 64000, "output": 8192},
                    "cost": {"input": 1, "output": 2}}}},
            "fixture-env": {"npm": "@ai-sdk/openai-compatible",
                "options": {"apiKey": "{env:FIXTURE_KEY}", "baseURL": args[4]},
                "models": {"fixture-model": {"name": "Env fixture"}}}
        }
    });
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, serde_json::to_vec_pretty(&original)?)?;
    let before = std::fs::read(&path)?;
    ensure!(
        config::migrate_provider_credentials_at(&path, |_, _| {
            Err(io::Error::other("simulated unavailable credential store"))
        })
        .is_err(),
        "Migration should fail closed"
    );
    ensure!(
        std::fs::read(&path)? == before,
        "Failed migration changed configuration"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let daemon = loop {
        match DaemonSupervisor::connect(&args[2], std::env::var("FINTWIND_TEST_DAEMON_TOKEN")?) {
            Ok(daemon) => break daemon,
            Err(error) if std::time::Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    };
    let store = StateStore::remote_in(daemon.clone(), &root.join("desktop"));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let save_key = |id: &str, key: &str| {
        store.store_custom_provider_key(
            binary.clone(),
            workspace.clone(),
            id.to_owned(),
            key.to_owned(),
        )
    };
    let result = (|| -> Result<Value> {
        config::migrate_provider_credentials_at(&path, save_key)?;
        let migrated: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        let mut expected = original;
        expected["provider"]["fixture"]["options"]
            .as_object_mut()
            .unwrap()
            .remove("apiKey");
        ensure!(
            migrated == expected,
            "Migration changed non-secret settings"
        );
        config::migrate_provider_credentials_at(&path, |_, _| {
            Err(io::Error::other(
                "Migration should not repeat credential writes",
            ))
        })?;

        let mut providers = config::load_providers_at(&path)?;
        let provider = providers
            .iter()
            .find(|p| p.id == "fixture")
            .unwrap()
            .clone();
        ensure!(
            provider.api_key.is_empty(),
            "The UI roster retained the migrated key"
        );
        // Await integration activation before probing, just like the settings page.
        store.fetch_integrations(binary.clone(), workspace.clone())?;
        let probe = provider.for_probe(&store, Some((binary.clone(), workspace.clone())))?;
        ensure!(
            probe.api_key == "fixture-old",
            "Selected credential did not resolve"
        );
        ensure!(
            !fintwind_client::custom_providers::fetch_api_model_list(&probe)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
                .is_empty(),
            "Authenticated model discovery failed"
        );

        store.authorize_provider(
            binary.clone(),
            workspace.clone(),
            "fixture".into(),
            "fixture-other".into(),
        )?;
        let integrations = store.fetch_integrations(binary.clone(), workspace.clone())?;
        let accounts = &integrations
            .iter()
            .find(|i| i.id == "fixture")
            .unwrap()
            .key_accounts;
        let external_id = accounts.iter().find(|a| a.active).unwrap().id.clone();
        ensure!(
            accounts.len() == 2,
            "Expected both Fintwind and OpenCode accounts"
        );

        providers
            .iter_mut()
            .find(|p| p.id == "fixture")
            .unwrap()
            .api_key = "fixture-new".into();
        let saved = config::save_providers_with_credentials_at(&path, &providers, None, save_key)?;
        ensure!(
            saved
                .iter()
                .find(|p| p.id == "fixture")
                .unwrap()
                .api_key
                .is_empty(),
            "Saved roster retained the replacement key"
        );
        ensure!(
            store
                .resolve_custom_provider_key(binary.clone(), workspace.clone(), "fixture".into())?
                .as_deref()
                == Some("fixture-new"),
            "Replacement credential did not activate"
        );
        let integrations = store.fetch_integrations(binary.clone(), workspace.clone())?;
        let accounts = &integrations
            .iter()
            .find(|i| i.id == "fixture")
            .unwrap()
            .key_accounts;
        ensure!(
            accounts.len() == 2 && accounts.iter().any(|a| a.id == external_id),
            "Rotation removed an external account"
        );
        let fintwind_id = accounts.iter().find(|a| a.active).unwrap().id.clone();
        store.activate_provider_key_account(
            binary.clone(),
            workspace.clone(),
            "fixture".into(),
            external_id,
        )?;
        ensure!(
            store
                .resolve_custom_provider_key(binary.clone(), workspace.clone(), "fixture".into())?
                .as_deref()
                == Some("fixture-other"),
            "Account switching did not change the selected key"
        );
        store.activate_provider_key_account(
            binary.clone(),
            workspace.clone(),
            "fixture".into(),
            fintwind_id,
        )?;
        config::save_providers_with_credentials_at(&path, &saved, None, |_, _| {
            Err(io::Error::other(
                "A model-only save must not write credentials",
            ))
        })?;
        let text = std::fs::read_to_string(&path)?;
        ensure!(
            !text.contains("fixture-old") && !text.contains("fixture-new"),
            "Configuration contains a literal key"
        );
        ensure!(
            text.contains("{env:FIXTURE_KEY}"),
            "Environment reference was removed"
        );
        let without = saved
            .iter()
            .filter(|p| p.id != "fixture")
            .cloned()
            .collect::<Vec<_>>();
        config::save_providers_with_credentials_at(&path, &without, None, save_key)?;
        config::save_providers_with_credentials_at(&path, &saved, None, save_key)?;
        ensure!(
            store
                .resolve_custom_provider_key(binary.clone(), workspace.clone(), "fixture".into())?
                .as_deref()
                == Some("fixture-new"),
            "Removing a definition deleted its credential"
        );
        Ok(
            json!({"passed": true, "checks": ["failed migration preserves file", "verified migration",
            "non-secret settings preserved", "idempotent migration", "authenticated discovery",
            "replacement credential", "external account preserved", "account switching", "model-only save", "environment reference", "deletion preserves credential"]}),
        )
    })();
    daemon.shutdown();
    result
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 5 {
        eprintln!("Use scripts/provider-credentials-check.py");
        std::process::exit(2);
    }
    let report = match check(&args) {
        Ok(report) => report,
        Err(error) => json!({"passed": false, "error": error.to_string()}),
    };
    std::fs::write(
        PathBuf::from(&args[1]).join("result.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
