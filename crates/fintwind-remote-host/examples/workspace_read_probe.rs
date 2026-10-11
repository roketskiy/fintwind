//! External-process harness for the complete project read boundary. It only
//! operates on an explicitly marked synthetic workspace. No production daemon,
//! user projects or provider credentials are used, and no unit tests are added.

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;

use anyhow::{Context as _, bail};
use fintwind_remote_host::workspace_diff::{DiffReadRequest, read_diff};
use fintwind_remote_host::workspace_directory::{DirectoryReadRequest, read_directory};
use fintwind_remote_host::workspace_reads::{FileReadRequest, read_file};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum Probe {
    ReadFile { request: FileReadRequest },
    ReadDirectory { request: DirectoryReadRequest },
    ReadDiff { request: DiffReadRequest },
}

fn main() -> anyhow::Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("marked synthetic root is required")?;
    if !root.is_absolute() || !root.join(".fintwind-e2e-fixture").is_file() {
        bail!("refusing to read an unmarked or relative workspace");
    }
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        let size = stdin
            .by_ref()
            .take(64 * 1024)
            .read_until(b'\n', &mut bytes)?;
        if size == 0 {
            break;
        }
        if size >= 64 * 1024 {
            bail!("probe request exceeds its limit");
        }
        let result = match serde_json::from_slice::<Probe>(&bytes) {
            Ok(Probe::ReadFile { request }) => {
                read_file(&root, &request).map(|page| serde_json::to_value(page).unwrap())
            }
            Ok(Probe::ReadDirectory { request }) => {
                read_directory(&root, &request).map(|page| serde_json::to_value(page).unwrap())
            }
            Ok(Probe::ReadDiff { request }) => {
                read_diff(&root, &request).map(|page| serde_json::to_value(page).unwrap())
            }
            Err(_) => Err((
                fintwind_remote_protocol::RejectCode::Malformed,
                "Invalid read request.".to_owned(),
            )),
        };
        let reply = match result {
            Ok(page) => serde_json::json!({"ok": true, "page": page}),
            Err((code, message)) => {
                serde_json::json!({"ok": false, "code": code, "message": message})
            }
        };
        writeln!(stdout, "{}", serde_json::to_string(&reply)?)?;
        stdout.flush()?;
    }
    Ok(())
}
