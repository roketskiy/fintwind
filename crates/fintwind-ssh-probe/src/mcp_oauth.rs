use anyhow::{Context, Result, ensure};
use std::ffi::OsString;

use crate::{DEADLINE, SshTarget, connect};

pub(super) async fn run(args: &[OsString]) -> Result<()> {
    ensure!(
        args.len() == 9,
        "usage: fintwind-ssh-probe --mcp-oauth HOST SSH_PORT USER PRIVATE_KEY KNOWN_HOSTS REMOTE_OPENCODE REMOTE_WORKSPACE MCP_NAME CALLBACK_PORT"
    );
    let target = SshTarget::from_args(args)?;
    let binary = args[5].to_str().context("invalid OpenCode path")?;
    let workspace = args[6].to_str().context("invalid workspace path")?;
    let name = args[7].to_str().context("invalid MCP name")?;
    let port = args[8]
        .to_string_lossy()
        .parse()
        .context("invalid callback port")?;
    let session = tokio::time::timeout(DEADLINE, connect(&target)).await??;
    fintwind_client::ssh::authenticate_mcp(session, binary, workspace, name, port, |url| {
        println!("{url}")
    })
    .await
}
