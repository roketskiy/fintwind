use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use rusqlite::Connection;
use uuid::{Uuid, Version};

pub(crate) fn installation_id(database: &Path) -> anyhow::Result<Uuid> {
    if let Some(parent) = database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).context("could not create activity database directory")?;
    }
    let connection = Connection::open(database).context("could not open activity database")?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS installation_identity (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            installation_id TEXT NOT NULL
        );",
    )?;
    connection.execute(
        "INSERT OR IGNORE INTO installation_identity (id, installation_id) VALUES (1, ?1)",
        [Uuid::new_v4().to_string()],
    )?;
    let stored: String = connection.query_row(
        "SELECT installation_id FROM installation_identity WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let identity = Uuid::parse_str(&stored).context("invalid installation identity")?;
    anyhow::ensure!(
        identity.get_version() == Some(Version::Random),
        "invalid installation identity version"
    );
    Ok(identity)
}

pub(crate) fn report_startup(database: &Path) -> anyhow::Result<()> {
    if cfg!(debug_assertions) {
        installation_id(database)?;
        return Ok(());
    }
    report_to(database, "https://fintwind.xyz/api/activity")
}

pub(crate) fn report_to(database: &Path, endpoint: &str) -> anyhow::Result<()> {
    let identity = installation_id(database)?;
    let body = serde_json::json!({ "installation_id": identity }).to_string();
    let (status, _, _) = fintwind_protocol::http::http_json(
        "POST",
        endpoint,
        &["Content-Type: application/json".to_owned()],
        Some(&body),
        10,
    )?;
    anyhow::ensure!(
        (200..300).contains(&status),
        "activity endpoint rejected request"
    );
    Ok(())
}
