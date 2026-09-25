//! End-to-end image recovery check against a real OpenCode user message.
//!
//! Build the debug daemon, then run:
//! cargo run --locked -p fintwind-core --example verify_native_image -- \
//!   <opencode-exe> <daemon-exe> <workspace> <session-id> <image-path>
use std::path::PathBuf;

use fintwind_client::{Command, DaemonSupervisor, ResponsePayload};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let binary = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing opencode binary"))?;
    let daemon_binary = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing daemon binary"))?;
    let workspace = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing workspace"))?;
    let session_id = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing session id"))?
        .to_string_lossy()
        .into_owned();
    let expected_path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing image path"))?;
    let expected = std::fs::read(&expected_path)?;
    let filename = expected_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid image path"))?
        .to_string_lossy();

    let daemon = DaemonSupervisor::spawn(&daemon_binary, false)?;
    let transcript = fintwind_client::persistence::StateStore::remote(daemon.clone())
        .fetch_native_transcript(binary, workspace, session_id)?;
    let mut matched = false;
    for attachment in transcript
        .messages
        .iter()
        .flat_map(|message| &message.attachments)
        .filter(|attachment| attachment.name == filename)
    {
        let Some(reference) = attachment.blob_reference.as_ref() else {
            continue;
        };
        let payload = daemon.client().request(
            Uuid::nil(),
            Uuid::nil(),
            Command::ReadBlob {
                reference: reference.clone(),
            },
        )?;
        if let ResponsePayload::BlobData { bytes } = payload
            && bytes == expected
        {
            matched = true;
            println!(
                "[OK] recovered {} ({} bytes) via daemon transcript and blob RPC",
                filename,
                bytes.len()
            );
            break;
        }
    }
    anyhow::ensure!(
        matched,
        "image bytes missing or different in recovered transcript"
    );
    daemon.shutdown();
    Ok(())
}
