//! Black-box acceptance for the daemon's mermaid check endpoint.
//!
//! Run:
//!
//! ```text
//! cargo run --locked --offline --package fintwind-core --example mermaid_check_e2e -- C:/Users/Public/Temp/opencode/mermaid-check-report.json
//! ```
//!
//! This runs the production `fintwind_core::server::serve` on a loopback port
//! exactly as `fintwind-daemon` does, so the real handshake routing, the real
//! restricted `/v1/mermaid-check` endpoint (`mermaid_check::handle`), the real
//! token check and the real shared compiler in `fintwind-diagram` all sit on
//! the path. The client is a plain WebSocket speaking the exact wire the
//! private OpenCode plugin speaks.
//!
//! The report carries no clock, port, path or machine data, so two runs on a
//! fixed compiler produce byte-identical files. Exit code is non-zero when a
//! case fails.

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tungstenite::Message;
use tungstenite::client::connect_with_config;
use tungstenite::protocol::WebSocketConfig;
use uuid::Uuid;

use fintwind_core::{Backend, EventSink, Request, ResponsePayload, ServerOptions, serve};
use fintwind_protocol::mermaid::{
    MERMAID_CHECK_ENDPOINT, MERMAID_CHECK_VERSION, MermaidCheckMessage, MermaidCheckReply,
};

/// One checked expectation. `detail` is bounded JSON for a failing diff.
#[derive(Serialize)]
struct Case {
    name: &'static str,
    passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct Report {
    tool: &'static str,
    cases: Vec<Case>,
    passed: usize,
    failed: usize,
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "mermaid-check-report.json".to_owned());
    let mut report = Report {
        tool: "mermaid_check_e2e",
        cases: Vec::new(),
        passed: 0,
        failed: 0,
    };
    let result = std::panic::catch_unwind(run);
    match result {
        Ok(Ok(cases)) => report.cases = cases,
        Ok(Err(error)) => report.cases.push(Case {
            name: "harness",
            passed: false,
            detail: Some(serde_json::json!({ "error": format!("{error:#}") })),
        }),
        Err(_) => report.cases.push(Case {
            name: "harness",
            passed: false,
            detail: Some(serde_json::json!({ "error": "the harness panicked" })),
        }),
    }
    for case in &report.cases {
        if case.passed {
            report.passed += 1;
        } else {
            report.failed += 1;
            println!(
                "FAIL {}: {}",
                case.name,
                case.detail
                    .as_ref()
                    .map(serde_json::Value::to_string)
                    .unwrap_or_default()
            );
        }
    }
    let summary = format!(
        "mermaid_check_e2e: {} cases, {} failed; report {path}",
        report.cases.len(),
        report.failed
    );
    let text = serde_json::to_string_pretty(&report).expect("the report is serializable");
    std::fs::write(&path, text).expect("the report path is writable");
    println!("{summary}");
    if report.failed > 0 {
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<Vec<Case>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?.to_string();
    let backend: Arc<dyn Backend> = Arc::new(StubBackend);
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let listener = listener;
        let backend = backend.clone();
        let shutdown = shutdown.clone();
        std::thread::Builder::new()
            .name("mermaid-check-e2e-serve".into())
            .spawn(move || {
                let _ = serve(listener, Uuid::new_v4().simple().to_string(), backend, shutdown, ServerOptions::default());
            })?;
    }

    // The endpoint capability is installed inside `serve`; poll for it the
    // same way a driver waits for the listener to come up.
    let mut environment = None;
    for _ in 0..300 {
        let candidate = fintwind_core::mermaid_check_environment();
        if !candidate.is_empty() {
            environment = Some(candidate);
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let environment = environment.ok_or_else(|| anyhow::anyhow!("the endpoint never installed"))?;
    let mut cases: Vec<Case> = Vec::new();

    let (address_env, address_value) = environment[0].clone();
    let (token_env, token_value) = environment[1].clone();
    cases.push(Case {
        name: "environment_shape",
        passed: address_env == "FINTWIND_MERMAID_CHECK_ADDRESS"
            && token_env == "FINTWIND_MERMAID_CHECK_TOKEN"
            && address_value == format!("ws://{address}{MERMAID_CHECK_ENDPOINT}")
            && token_value.len() >= 32,
        // The loopback port is ephemeral; the report stays deterministic by
        // recording only the endpoint path and the token length.
        detail: Some(serde_json::json!({
            "endpoint": MERMAID_CHECK_ENDPOINT,
            "tokenBytes": token_value.len(),
        })),
    });
    let url = address_value;

    // A wrong token is refused with a rejection, never a hello.
    {
        let mut socket = connect(&url)?;
        send(&mut socket, &MermaidCheckMessage::Hello {
            version: MERMAID_CHECK_VERSION,
            token: "not-the-token".into(),
        })?;
        let reply = read_reply(&mut socket)?;
        cases.push(Case {
            name: "wrong_token_rejected",
            passed: matches!(reply, MermaidCheckReply::Rejected { .. }),
            detail: Some(serde_json::json!({ "reply": format!("{reply:?}") })),
        });
    }

    // The right token authenticates, and one connection serves many checks.
    let mut socket = connect(&url)?;
    send(&mut socket, &MermaidCheckMessage::Hello {
        version: MERMAID_CHECK_VERSION,
        token: token_value.clone(),
    })?;
    let reply = read_reply(&mut socket)?;
    cases.push(Case {
        name: "hello_ok",
        passed: matches!(reply, MermaidCheckReply::Hello { version: MERMAID_CHECK_VERSION }),
        detail: Some(serde_json::json!({ "reply": format!("{reply:?}") })),
    });

    let valid = "\
flowchart LR
  A[Build] --> B{Pass?}
  B -->|yes| C[Ship]
  B -->|no| A
";
    let (ok, error, width, height) = check(&mut socket, valid)?;
    cases.push(Case {
        name: "valid_flowchart",
        passed: ok && error.is_none() && width.unwrap_or(0.0) > 0.0 && height.unwrap_or(0.0) > 0.0,
        detail: Some(serde_json::json!({ "ok": ok, "error": error, "width": width, "height": height })),
    });

    let invalid = "\
flowchart LR
  A[Start] --> ->
";
    let (ok, error, width, _height) = check(&mut socket, invalid)?;
    cases.push(Case {
        name: "invalid_source_reports_location",
        // Both location forms are legitimate: `at line N` when only the line
        // is known, `at line:column` otherwise.
        passed: !ok
            && error.as_deref().is_some_and(|text| text.contains(" at "))
            && width.is_none(),
        detail: Some(serde_json::json!({ "ok": ok, "error": error })),
    });

    let unsupported = "gitGraph\n  commit id: \"one\"\n";
    let (ok, error, _, _) = check(&mut socket, unsupported)?;
    // Ondine may or may not support git graphs; the contract is that the
    // answer is well formed: a pass carries no error and a failure carries
    // exactly one.
    cases.push(Case {
        name: "answers_are_well_formed",
        passed: ok != error.is_some(),
        detail: Some(serde_json::json!({ "ok": ok, "error": error })),
    });

    let oversized = format!("flowchart LR\n  A{}[x]\n", "L".repeat(33 * 1024));
    let (ok, error, width, height) = check(&mut socket, &oversized)?;
    cases.push(Case {
        name: "oversized_source_refused",
        passed: !ok
            && error.as_deref().is_some_and(|text| text.contains("limit")),
        detail: Some(serde_json::json!({ "ok": ok, "error": error, "width": width, "height": height })),
    });

    // The same connection answers a second check: no per-check handshake.
    let (ok, _, _, _) = check(&mut socket, valid)?;
    cases.push(Case {
        name: "second_check_on_same_connection",
        passed: ok,
        detail: Some(serde_json::json!({ "ok": ok })),
    });

    // Close the connection the way the plugin would, so the daemon side logs
    // no protocol error.
    let _ = socket.send(Message::Close(None));
    shutdown.store(true, Ordering::Release);
    Ok(cases)
}

struct StubBackend;

impl Backend for StubBackend {
    fn handle(&self, _request: Request, _events: EventSink) -> anyhow::Result<ResponsePayload> {
        anyhow::bail!("the mermaid check e2e never drives the desktop protocol")
    }
}

fn connect(url: &str) -> anyhow::Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(64 * 1024))
        .max_frame_size(Some(64 * 1024));
    let (socket, _) = connect_with_config(url, Some(config), 3)
        .map_err(|error| anyhow::anyhow!("could not open {url}: {error}"))?;
    Ok(socket)
}

fn send(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    message: &MermaidCheckMessage,
) -> anyhow::Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(message)?.into()))
        .map_err(|error| anyhow::anyhow!("could not send: {error}"))
}

fn read_reply(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
) -> anyhow::Result<MermaidCheckReply> {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                return Ok(serde_json::from_str(&text)?);
            }
            Ok(Message::Ping(_)) => continue,
            Ok(_) => continue,
            Err(error) => return Err(anyhow::anyhow!("read failed: {error}")),
        }
    }
}

/// One `check` round trip on an authenticated connection.
fn check(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    source: &str,
) -> anyhow::Result<(bool, Option<String>, Option<f64>, Option<f64>)> {
    send(socket, &MermaidCheckMessage::Check {
        request_id: Uuid::new_v4(),
        source: source.to_owned(),
    })?;
    loop {
        match read_reply(socket)? {
            MermaidCheckReply::Result {
                request_id: _,
                ok,
                error,
                width,
                height,
            } => return Ok((ok, error, width, height)),
            MermaidCheckReply::Rejected { message } => {
                return Ok((false, Some(message), None, None));
            }
            MermaidCheckReply::Hello { .. } => continue,
        }
    }
}
