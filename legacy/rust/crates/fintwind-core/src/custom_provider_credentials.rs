//! Custom provider keys belong to OpenCode, not its configuration file.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::opencode_session::{OpenCodeServer, encode_path_segment};

fn credentials(server: &OpenCodeServer) -> Result<Vec<Value>> {
    let response = server
        .request("GET", "/api/credential", None)
        .map_err(|_| anyhow::anyhow!("Could not read OpenCode credentials"))?;
    response
        .get("data")
        .unwrap_or(&response)
        .as_array()
        .cloned()
        .context("OpenCode returned an invalid credential list")
}

pub(crate) fn store(server: &OpenCodeServer, provider: &str, key: &str) -> Result<()> {
    ensure!(
        !provider.is_empty() && !key.trim().is_empty(),
        "Missing provider or API key"
    );
    let prefix = format!("fintwind-custom:{provider}:");
    let rows = credentials(server)?;
    let value = json!({"type": "key", "key": key});
    let owned = rows
        .iter()
        .filter(|row| {
            row["id"].as_str().is_some_and(|id| id.starts_with(&prefix))
                && row["integrationID"].as_str() == Some(provider)
                && row["value"]["type"].as_str() == Some("key")
        })
        .collect::<Vec<_>>();
    let existing = owned.iter().find(|row| row["value"] == value);
    let id = existing
        .and_then(|row| row["id"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{prefix}{}", uuid::Uuid::new_v4()));
    if let Some(existing) = existing {
        if existing["active"].as_bool() != Some(true) {
            server
                .request(
                    "POST",
                    &format!("/api/credential/{}/activate", encode_path_segment(&id)),
                    None,
                )
                .map_err(|_| anyhow::anyhow!("Could not activate the OpenCode credential"))?;
        }
    } else {
        server
            .request(
                "POST",
                "/api/credential",
                Some(
                    &json!({"id": id, "integrationID": provider, "label": "Fintwind",
                    "value": value, "activate": true}),
                ),
            )
            .map_err(|_| anyhow::anyhow!("Could not save the OpenCode credential"))?;
    }
    // Do not acknowledge until the server confirms persistence and selection.
    // Errors intentionally omit response bodies because they may contain keys.
    ensure!(
        credentials(server)?.iter().any(|row| {
            row["id"].as_str() == Some(&id)
                && row["integrationID"].as_str() == Some(provider)
                && row["active"].as_bool() == Some(true)
                && row["value"] == value
        }),
        "OpenCode did not confirm the saved credential"
    );
    // OpenCode's PATCH route only renames labels. Rotate by creating and
    // verifying first; never delete the old credential before its replacement
    // is usable, and never delete accounts created outside this namespace.
    for previous in owned {
        if let Some(previous_id) = previous["id"].as_str().filter(|previous| *previous != id) {
            server
                .request(
                    "DELETE",
                    &format!("/api/credential/{}", encode_path_segment(previous_id)),
                    None,
                )
                .map_err(|_| {
                    anyhow::anyhow!("Could not remove the superseded Fintwind credential")
                })?;
        }
    }
    Ok(())
}

pub(crate) fn activate(server: &OpenCodeServer, provider: &str, id: &str) -> Result<()> {
    ensure!(
        credentials(server)?
            .iter()
            .any(|row| row["id"].as_str() == Some(id)
                && row["integrationID"].as_str() == Some(provider)
                && row["value"]["type"].as_str() == Some("key")),
        "Provider key account not found"
    );
    server
        .request(
            "POST",
            &format!("/api/credential/{}/activate", encode_path_segment(id)),
            None,
        )
        .map_err(|_| anyhow::anyhow!("Could not activate the provider account"))?;
    Ok(())
}

pub(crate) fn resolve(server: &OpenCodeServer, provider: &str) -> Result<Option<String>> {
    let response = server
        .request(
            "GET",
            &format!("/api/integration/{}", encode_path_segment(provider)),
            None,
        )
        .map_err(|_| anyhow::anyhow!("Could not read the provider connection"))?;
    let integration = response.get("data").unwrap_or(&response);
    ensure!(
        integration["id"].as_str() == Some(provider),
        "Unexpected provider connection"
    );
    let Some(connection) = integration["connections"]
        .as_array()
        .and_then(|rows| rows.first())
    else {
        return Ok(None);
    };
    match connection["type"].as_str() {
        Some("env") => {
            let name = connection["name"]
                .as_str()
                .context("Missing credential environment name")?;
            Ok(Some(std::env::var(name).context(
                "The provider credential environment is unavailable",
            )?))
        }
        Some("credential") => {
            let rows = credentials(server)?;
            let row = rows
                .iter()
                .find(|row| {
                    row["id"] == connection["id"] && row["integrationID"].as_str() == Some(provider)
                })
                .context("The selected provider credential is missing")?;
            ensure!(
                row["value"]["type"].as_str() == Some("key"),
                "This provider connection does not support direct API-key probes"
            );
            Ok(Some(
                row["value"]["key"]
                    .as_str()
                    .context("Missing provider API key")?
                    .to_owned(),
            ))
        }
        _ => anyhow::bail!("Unsupported provider connection"),
    }
}
