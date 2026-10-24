//! Daemon-only provider discovery layered over shared protocol models.

use std::path::Path;
use std::time::Duration;

pub use fintwind_protocol::model::*;

/// Settings paths refer to files on the daemon host, independently of its cwd.
pub fn validate_opencode_binary_path(value: &str) -> anyhow::Result<std::path::PathBuf> {
    let path = Path::new(value.trim());
    anyhow::ensure!(
        path.is_absolute() && path.is_file(),
        "OpenCode executable must be an existing absolute file path"
    );
    #[cfg(windows)]
    anyhow::ensure!(
        path.extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| ["exe", "com", "cmd", "bat"]
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))),
        "OpenCode executable must be an .exe, .com, .cmd, or .bat file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            std::fs::metadata(path)?.permissions().mode() & 0o111 != 0,
            "OpenCode file is not executable"
        );
    }
    Ok(path.to_owned())
}

pub fn provider_probe(
    settings: &crate::DaemonSettings,
    binary_override: Option<&str>,
) -> ProviderProbe {
    let binary_override = binary_override.or_else(|| settings.opencode_binary_path());
    let path = match binary_override {
        Some(binary) => crate::command_env::resolve_binary_override(binary),
        None => crate::command_env::find_executable("opencode"),
    };
    ProviderProbe {
        installed: path.is_some(),
        path,
        models: crate::model_catalog::fallback_models(),
        agent_presets: crate::model_catalog::fallback_agent_presets(),
    }
}

/// Detect the provider and hydrate its catalog from the daemon-owned cache.
///
/// This is the fast half of stale-while-revalidate: clients can render the
/// last successful catalog immediately, then request live discovery to replace
/// it. Cache I/O stays in the daemon instead of leaking host filesystem access
/// into desktop or Web clients.
pub fn cached_provider_probe(
    settings: &crate::DaemonSettings,
    binary_override: Option<&str>,
    directory: Option<&Path>,
) -> ProviderProbe {
    let cached = crate::model_catalog::cached_models(directory);
    apply_cached_models(provider_probe(settings, binary_override), cached)
}

fn apply_cached_models(
    mut probe: ProviderProbe,
    cached_models: Option<Vec<ProviderModel>>,
) -> ProviderProbe {
    if let Some(models) = cached_models {
        probe.models = models;
    }
    probe
}

pub fn discover_provider_models(
    mut probe: ProviderProbe,
    directory: Option<&Path>,
) -> ProviderProbe {
    if let Some(path) = probe.path.as_deref() {
        let (models, agent_presets) = crate::model_catalog::discover_catalog(path, directory);
        probe.models = models;
        probe.agent_presets = agent_presets;
    }
    probe
}

/// Run `opencode --version` on the daemon host and extract its first
/// version-like token. The CLI decorates this output differently, so clients
/// receive a normalized value rather than subprocess output.
pub fn probe_provider_version(binary: &Path) -> Option<String> {
    let mut command = crate::command_env::command(binary);
    let command = command.arg("--version");
    let output = crate::command_env::output_with_timeout(command, Duration::from_secs(5))
        .ok()
        .flatten()?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_cli_version(&combined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_catalog_replaces_fallback_before_live_discovery() {
        let probe = ProviderProbe {
            installed: true,
            path: Some("/usr/bin/opencode".into()),
            models: crate::model_catalog::fallback_models(),
            agent_presets: Vec::new(),
        };
        let cached = vec![ProviderModel::new("cached-model", "Cached model").default()];

        let probe = apply_cached_models(probe, Some(cached));

        assert_eq!(probe.models.len(), 1);
        assert_eq!(probe.models[0].id, "cached-model");
    }
}
