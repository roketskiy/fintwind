//! Run once with scripts/provider-credentials-check.py and isolated binaries.
use anyhow::{Result, ensure};
use fintwind_client::{DaemonSupervisor, opencode_config as config, persistence::StateStore};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn document(path: &std::path::Path) -> Result<Value> {
    let text = std::fs::read_to_string(path)?;
    Ok(
        jsonc_parser::cst::CstRootNode::parse(&text, &Default::default())?
            .to_serde_value()
            .unwrap(),
    )
}

fn check(args: &[String]) -> Result<Value> {
    let root = PathBuf::from(&args[1]);
    let path = config::config_path().with_extension("jsonc");
    std::fs::create_dir_all(path.parent().unwrap())?;
    let original = json!({
        "$schema": "https://opencode.ai/config.json", "experimental": {"image_generate": {"model": "fixture/image"}},
        "provider": {
            "fixture": {"npm": "@ai-sdk/openai", "options": {"baseURL": args[4], "apiKey": "fixture-old", "headers": {"X-Fixture": "preserved"}},
                "models": {
                    "chat": {"limit": {"context": 64000, "output": 8192}, "options": {"reasoningEffort": "low"}, "variants": {"fast": {"reasoningEffort": "low"}}, "cost": {"input": 1, "output": 2}},
                    "image": {"modalities": {"input": ["text", "image"], "output": ["image"]}},
                    "image-url": {"modalities": {"output": ["text"]}}
                }},
            "unknown": {"npm": "@ai-sdk/anthropic", "models": {"image": {"modalities": {"output": ["image"]}}}}
        },
        "providers": {"native": {"package": "@opencode/ai/providers/openai", "settings": {"baseURL": args[4], "apiKey": "fixture-old"},
            "models": {"existing": {"type": "image", "package": "@opencode/ai/providers/openai/images", "body": {"quality": "low"}}}}}
    });
    std::fs::write(
        &path,
        format!(
            "// preserve this comment\n{}\n",
            serde_json::to_string_pretty(&original)?
        ),
    )?;
    ensure!(
        config::migrate_image_models_at(&path)?,
        "Legacy image model was not repaired"
    );
    let migrated = document(&path)?;
    ensure!(migrated["providers"]["fixture"]["models"]["image"]["type"] == "image");
    ensure!(
        migrated["providers"]["fixture"]["models"]["image"]["package"]
            == "@opencode/ai/providers/openai/images"
    );
    ensure!(
        migrated["providers"]["fixture"]["package"] == "aisdk:@ai-sdk/openai",
        "Chat protocol changed"
    );
    ensure!(migrated["providers"]["fixture"]["settings"]["apiKey"] == "fixture-old");
    ensure!(migrated["providers"]["fixture"]["settings"]["baseURL"] == args[4]);
    ensure!(migrated["providers"]["fixture"]["headers"]["X-Fixture"] == "preserved");
    ensure!(migrated["providers"]["fixture"]["models"]["chat"]["limit"]["context"] == 64000);
    ensure!(
        migrated["providers"]["fixture"]["models"]["chat"]["variants"][0]["settings"]["reasoningEffort"]
            == "low"
    );
    ensure!(migrated["providers"]["fixture"]["models"]["chat"]["cost"][0]["output"] == 2);
    ensure!(migrated["provider"]["unknown"] == original["provider"]["unknown"]);
    ensure!(migrated["providers"]["native"] == original["providers"]["native"]);
    ensure!(migrated["experimental"] == original["experimental"]);
    let before = std::fs::read(&path)?;
    ensure!(
        !config::migrate_image_models_at(&path)?,
        "Migration was not idempotent"
    );
    ensure!(std::fs::read(&path)? == before);
    ensure!(String::from_utf8(before)?.contains("// preserve this comment"));

    let mut providers = config::load_providers_at(&path)?;
    let fixture = providers.iter_mut().find(|p| p.id == "fixture").unwrap();
    fixture
        .models
        .iter_mut()
        .find(|m| m.id == "chat")
        .unwrap()
        .api_package = Some("@opencode/ai/providers/anthropic".into());
    config::save_providers_at(&path, &providers, None)?;
    let saved = document(&path)?;
    ensure!(
        saved["providers"]["fixture"]["models"]["chat"]["package"]
            == "@opencode/ai/providers/anthropic"
    );
    ensure!(saved["providers"]["fixture"]["models"]["chat"]["type"] == "chat");
    ensure!(saved["providers"]["native"]["models"]["existing"]["body"]["quality"] == "low");
    let mut providers = config::load_providers_at(&path)?;
    let fixture = providers.iter_mut().find(|p| p.id == "fixture").unwrap();
    fixture
        .models
        .iter_mut()
        .find(|m| m.id == "chat")
        .unwrap()
        .api_package = None;
    config::save_providers_at(&path, &providers, None)?;
    ensure!(
        document(&path)?["providers"]["fixture"]["models"]["chat"]
            .get("package")
            .is_none()
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    let daemon = loop {
        match DaemonSupervisor::connect(&args[2], std::env::var("FINTWIND_TEST_DAEMON_TOKEN")?) {
            Ok(daemon) => break daemon,
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    let store = StateStore::remote_in(daemon, &root.join("desktop"));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let binary = PathBuf::from(&args[3]);
    let images = store.fetch_image_models(binary.clone(), workspace.clone())?;
    ensure!(
        images.iter().any(|m| m.id == "fixture/image"),
        "Repaired model missing from runtime image inventory"
    );
    ensure!(
        !images.iter().any(|m| m.id == "fixture/chat"
            || m.id == "fixture/image-url"
            || m.id == "unknown/image")
    );
    let mut providers = config::load_providers_at(&path)?;
    providers
        .iter_mut()
        .find(|p| p.id == "fixture")
        .unwrap()
        .models
        .retain(|m| m.id != "image");
    config::save_providers_at(&path, &providers, None)?;
    store.reload_opencode_server(binary.clone(), workspace.clone())?;
    ensure!(
        !store
            .fetch_image_models(binary, workspace)?
            .iter()
            .any(|m| m.id == "fixture/image"),
        "Removed image model remained available"
    );
    Ok(
        json!({"passed": true, "checks": ["legacy repair", "chat settings preserved", "native override preserved", "unknown protocol untouched", "JSONC and idempotence", "model override and inheritance", "live image inventory", "model deletion"], "paidRequests": 0}),
    )
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(args.len() == 5, "Use scripts/provider-credentials-check.py");
    let result = check(&args);
    let report = match &result {
        Ok(report) => report.clone(),
        Err(error) => json!({"passed": false, "error": format!("{error:#}")}),
    };
    std::fs::write(
        PathBuf::from(&args[1]).join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    result.map(|_| ())
}
