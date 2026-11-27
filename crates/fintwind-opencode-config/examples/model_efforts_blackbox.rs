//! Black-box check of the bundled models.dev snapshot and the recorded
//! thinking-effort round trip: save writes the UI-owned `reasoningEfforts`
//! list plus exactly the selected variants, load reads the list back, and
//! clearing it restores the catalog/ladder behavior. Run with:
//! `cargo run -p fintwind-opencode-config --example model_efforts_blackbox`.
use anyhow::{Result, ensure};
use fintwind_opencode_config::{
    custom_providers::{CustomProvider, CustomProviderModel, ProviderApiFormat},
    models_dev, opencode_config as config,
};
use serde_json::Value;

fn main() -> Result<()> {
    let root = std::env::temp_dir().join(format!("fintwind-efforts-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    let path = root.join("opencode.json");

    // The bundled snapshot answers offline: a recent vendor model carries a
    // context window and thinking modes without any network or cache.
    let table = models_dev::bundled_catalog();
    let basics = table.resolve_model_basics_for("gpt-5", Some("openai"));
    ensure!(basics.context_window.is_some_and(|w| w > 0), "bundled context");
    let catalog_modes = table
        .resolve_thinking_for("gpt-5", Some("openai"))
        .expect("bundled thinking modes");
    ensure!(!catalog_modes.modes.is_empty(), "bundled modes");

    let recorded = CustomProviderModel {
        id: "fixture-model".into(),
        reasoning_efforts: vec!["none".into(), "low".into(), "high".into()],
        ..Default::default()
    };
    let unrecorded = CustomProviderModel {
        id: "gpt-5".into(),
        ..Default::default()
    };
    let provider = CustomProvider::new(
        "fixture".into(),
        "Fixture".into(),
        "https://fixture.example/v1".into(),
        ProviderApiFormat::OpenAi,
        String::new(),
        vec![recorded, unrecorded],
    );
    config::save_providers_at(&path, &[provider], Some(&table))?;

    let saved: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let spec = &saved["provider"]["fixture"]["models"]["fixture-model"];
    ensure!(
        spec["reasoningEfforts"] == serde_json::json!(["none", "low", "high"]),
        "recorded levels ride as a UI-owned list"
    );
    let mut keys: Vec<&str> = spec["variants"]
        .as_object()
        .expect("variants written")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    ensure!(keys == ["high", "low", "nothinking"], "selection is authoritative");
    ensure!(
        saved["provider"]["fixture"]["models"]["gpt-5"]
            .get("reasoningEfforts")
            .is_none(),
        "unrecorded model keeps no list"
    );

    // Load reads the recorded list back; the unrecorded model stays empty.
    let loaded = config::load_providers_at(&path)?;
    let fixture = loaded.iter().find(|p| p.id == "fixture").expect("fixture");
    ensure!(
        fixture.model("fixture-model").unwrap().reasoning_efforts
            == vec!["none".to_string(), "low".to_string(), "high".to_string()],
        "efforts round-trip"
    );
    ensure!(
        fixture.model("gpt-5").unwrap().reasoning_efforts.is_empty(),
        "unrecorded stays unset"
    );

    // Clearing the selection drops the list and restores the ladder for a
    // model the catalog does not describe.
    let mut cleared = loaded;
    cleared
        .iter_mut()
        .find(|p| p.id == "fixture")
        .unwrap()
        .models
        .iter_mut()
        .find(|m| m.id == "fixture-model")
        .unwrap()
        .reasoning_efforts
        .clear();
    config::save_providers_at(&path, &cleared, Some(&table))?;
    let saved: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let spec = &saved["provider"]["fixture"]["models"]["fixture-model"];
    ensure!(spec.get("reasoningEfforts").is_none(), "cleared list removed");
    let mut keys: Vec<&str> = spec["variants"]
        .as_object()
        .expect("ladder written")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    ensure!(
        keys == ["high", "low", "max", "medium", "nothinking", "xhigh"],
        "standard ladder restored"
    );

    std::fs::remove_dir_all(&root)?;
    println!("model_efforts_blackbox: PASS");
    Ok(())
}
