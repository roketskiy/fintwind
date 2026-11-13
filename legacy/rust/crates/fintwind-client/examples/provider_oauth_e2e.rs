//! End-to-end check of the provider OAuth surface against a real OpenCode
//! server. Spawns an isolated `fintwind-daemon` (own data directory, random
//! token), points it at the local `opencode` binary, and verifies the
//! integration roster's OAuth methods — the exact shapes the app's in-app
//! sign-in flow connects through.
//!
//! Read-only: no connect, no credential writes, no browser flows. The only
//! processes launched are this run's own daemon and OpenCode server, and
//! both are stopped on exit. Every wait is bounded: readiness 60 s, daemon
//! handshake 15 s, the roster RPC 90 s (the daemon-side HTTP timeout).
//!
//! Usage:
//!   cargo run -p fintwind-client --example provider_oauth_e2e -- \
//!     <path-to-fintwind-daemon.exe> [opencode-binary]

use std::io::BufRead;
use std::process::{Command as StdCommand, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, bail};
use fintwind_protocol::{DAEMON_TOKEN_ENV, Command, ResponsePayload};
use uuid::Uuid;

const READY_TIMEOUT: Duration = Duration::from_secs(60);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const RPC_TIMEOUT: Duration = Duration::from_secs(90);

fn main() -> anyhow::Result<()> {
    let Some(daemon_path) = std::env::args_os().nth(1) else {
        bail!("usage: provider_oauth_e2e <fintwind-daemon.exe> [opencode-binary]");
    };
    let opencode_path = std::env::args_os()
        .nth(2)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("opencode"));

    let data_dir = std::env::temp_dir()
        .join(format!("fintwind-oauth-e2e-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&data_dir).context("could not create the isolated data directory")?;
    let token = Uuid::new_v4().to_string();

    println!("[1/4] spawning isolated daemon at {}", data_dir.display());
    let mut daemon = StdCommand::new(&daemon_path)
        .args([
            "--bind",
            "127.0.0.1:0",
            // This run owns the daemon; the parent id lets it answer the
            // graceful Shutdown request the check sends when it is done.
            "--parent-pid",
            &std::process::id().to_string(),
            "--data-dir",
        ])
        .arg(&data_dir)
        .env(DAEMON_TOKEN_ENV, &token)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("could not spawn fintwind-daemon")?;

    let result = run_check(&mut daemon, &token, &opencode_path, &data_dir);
    let outcome = match &result {
        Ok(()) => "passed",
        Err(_) => "failed",
    };
    println!("[4/4] stopping this run's daemon (pid {})", daemon.id());
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&data_dir);
    println!("provider_oauth_e2e: {outcome}");
    result
}

fn run_check(
    daemon: &mut std::process::Child,
    token: &str,
    opencode: &std::path::Path,
    workspace: &std::path::Path,
) -> anyhow::Result<()> {
    // The readiness line, with a deadline: a daemon that never announces
    // must not hang the check.
    let stdout = daemon.stdout.take().context("daemon stdout was piped")?;
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let line = std::io::BufReader::new(stdout)
            .lines()
            .next()
            .and_then(|line| line.ok());
        let _ = ready_tx.send(line);
    });
    let ready_line = ready_rx
        .recv_timeout(READY_TIMEOUT)
        .map_err(|_| anyhow::anyhow!("daemon printed no readiness line within {READY_TIMEOUT:?}"))?
        .context("daemon closed stdout before announcing readiness")?;
    let ready: serde_json::Value = serde_json::from_str(&ready_line)
        .with_context(|| format!("daemon readiness line is not JSON: {ready_line}"))?;
    let address = ready["address"]
        .as_str()
        .context("readiness line carries no address")?
        .to_owned();
    println!("[2/4] daemon ready at {address}");

    // The handshake, also with a deadline: connect runs on a worker and the
    // main thread waits bounded; on timeout the daemon is abandoned and the
    // caller's kill in main() ends it.
    let token = token.to_owned();
    let (client_tx, client_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let client = fintwind_client::DaemonClient::connect(&address, token);
        let _ = client_tx.send(client);
    });
    let client = client_rx
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .map_err(|_| anyhow::anyhow!("daemon handshake did not finish within {HANDSHAKE_TIMEOUT:?}"))?
        .context("daemon handshake failed")?;
    println!("[3/4] connected; asking for the integration roster (read-only)");

    let response = client
        .request_with_timeout(
            Uuid::nil(),
            Uuid::nil(),
            Command::FetchIntegrations {
                binary: opencode.to_owned(),
                directory: workspace.to_owned(),
            },
            RPC_TIMEOUT,
        )
        .map_err(|error| anyhow::anyhow!("fetch_integrations failed: {error}"))?;
    let integrations = match response {
        ResponsePayload::Integrations { integrations } => integrations,
        _ => bail!("daemon answered the roster request with the wrong payload"),
    };

    let mut passed: Vec<String> = Vec::new();
    passed.push(format!("integration rows: {}", integrations.len()));
    if integrations.is_empty() {
        bail!("the server reported no integrations");
    }

    // github-copilot: one `device` OAuth method whose form asks the
    // deployment type (two predefined options, required) and, conditionally,
    // the enterprise URL.
    let copilot = integrations
        .iter()
        .find(|integration| integration.id == "github-copilot")
        .context("no github-copilot integration")?;
    let device = copilot
        .oauth_methods
        .iter()
        .find(|method| method.id == "device")
        .context("github-copilot has no `device` OAuth method")?;
    let deployment = device
        .form
        .iter()
        .find(|field| field.key == "deploymentType")
        .context("copilot form has no deploymentType field")?;
    assert!(
        deployment.required && deployment.options.len() == 2,
        "deploymentType must be a required choice of two"
    );
    assert!(
        device
            .form
            .iter()
            .any(|field| field.key == "enterpriseUrl" && !field.when.is_empty()),
        "enterpriseUrl must carry a visibility condition"
    );
    passed.push("github-copilot device form: [OK]".to_owned());

    // opencode Console: the OAuth method's hidden `server` field must ride
    // the model (the start request folds its default in silently).
    let console = integrations
        .iter()
        .find(|integration| integration.id == "opencode")
        .context("no opencode integration")?;
    let console_device = console
        .oauth_methods
        .iter()
        .find(|method| method.id == "device")
        .context("opencode has no `device` OAuth method")?;
    let server_field = console_device
        .form
        .iter()
        .find(|field| field.key == "server")
        .context("opencode form has no hidden server field")?;
    assert!(
        server_field.hidden && server_field.default.is_some(),
        "opencode's server field must be hidden with a default"
    );
    passed.push("opencode console hidden default: [OK]".to_owned());

    // openai: several OAuth methods, each carrying a server label — the
    // form-less flow picks them per button.
    let openai = integrations
        .iter()
        .find(|integration| integration.id == "openai")
        .context("no openai integration")?;
    assert!(
        openai.oauth_methods.len() >= 2,
        "openai must offer multiple OAuth methods"
    );
    assert!(
        openai
            .oauth_methods
            .iter()
            .all(|method| !method.label.is_empty()),
        "every openai OAuth method needs a label"
    );
    passed.push(format!(
        "openai OAuth methods: {} [OK]",
        openai.oauth_methods.len()
    ));

    // A method without any form must be ready to start directly.
    assert!(
        integrations
            .iter()
            .any(|integration| integration
                .oauth_methods
                .iter()
                .any(|method| method.form.is_empty())),
        "expected at least one form-less OAuth method"
    );
    passed.push("form-less method present: [OK]".to_owned());

    for line in passed {
        println!("PASS {line}");
    }

    // Ask the daemon to shut down gracefully before the caller's kill: the
    // connection reader then sees `ShuttingDown` instead of a torn socket.
    client.shutdown();
    Ok(())
}
