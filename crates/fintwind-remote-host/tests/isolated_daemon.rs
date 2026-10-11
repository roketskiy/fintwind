//! Whole-process isolation smoke, not a unit test. Failure modes were listed
//! before adding --data-dir: test startup touches the user's default database,
//! legacy settings leak into an empty fixture, or a failed isolation directory
//! falls back silently. This launches the real daemon and its normal wire API.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command as ProcessCommand, Stdio};

use fintwind_client::DaemonClient;
use fintwind_protocol::{Command, ResponsePayload};
use uuid::Uuid;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn real_daemon_uses_only_the_explicit_data_directory() {
    let base = if cfg!(windows) {
        PathBuf::from("C:/Users/Public/Temp/opencode")
    } else {
        std::env::temp_dir()
    };
    let root = base.join(format!("remote-daemon-isolation-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create isolated artifact directory");
    let executable = std::env::var_os("FINTWIND_E2E_DAEMON_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug")
                .join(if cfg!(windows) {
                    "fintwind-daemon.exe"
                } else {
                    "fintwind-daemon"
                })
        });
    let token = Uuid::new_v4().to_string();
    let child = ProcessCommand::new(&executable)
        .args(["--bind", "127.0.0.1:0", "--data-dir"])
        .arg(&root)
        .env("FINTWIND_DAEMON_TOKEN", &token)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("build fintwind-daemon before running this process black box");
    let mut child = ChildGuard(child);
    let stdout = child.0.stdout.take().expect("ready stdout");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(read.map(|_| line));
    });
    let line = receiver
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("daemon startup is bounded")
        .expect("daemon ready line");
    let ready: fintwind_protocol::DaemonReady =
        serde_json::from_str(&line).expect("daemon ready envelope");
    let client =
        DaemonClient::connect(&ready.address, token).expect("authenticate isolated daemon");
    let reply = client
        .request(Uuid::nil(), Uuid::nil(), Command::LoadTaskState)
        .expect("load isolated state");
    let ResponsePayload::TaskState {
        projects, sessions, ..
    } = reply
    else {
        panic!("expected isolated task state");
    };
    assert!(
        projects.is_empty() && sessions.is_empty(),
        "new data directory must not load user projects"
    );
    assert!(
        root.join("app.db").is_file(),
        "daemon created its own database"
    );
    let report = serde_json::json!({
        "scenario": "real-daemon-explicit-data-directory",
        "passed": true,
        "defaultProfileLoaded": false,
        "projects": projects.len(),
        "sessions": sessions.len(),
        "publicChainVerified": false
    });
    std::fs::write(
        root.join("summary.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .expect("write repeatable black-box artifact");
    println!("[ARTIFACT] {}", root.join("summary.json").display());
}

#[test]
fn invalid_isolation_paths_never_fall_back_or_announce_readiness() {
    let base = if cfg!(windows) {
        PathBuf::from("C:/Users/Public/Temp/opencode")
    } else {
        std::env::temp_dir()
    };
    let root = base.join(format!(
        "remote-daemon-invalid-isolation-{}",
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).expect("create unique artifact directory");
    let executable = std::env::var_os("FINTWIND_E2E_DAEMON_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug")
                .join(if cfg!(windows) {
                    "fintwind-daemon.exe"
                } else {
                    "fintwind-daemon"
                })
        });
    let file = root.join("not-a-directory");
    std::fs::write(&file, b"synthetic fixture\n").expect("create non-directory fixture");
    let mut scenarios = Vec::new();
    for (label, directory) in [
        ("relative-path", PathBuf::from("relative-isolation")),
        ("file-instead-of-directory", file),
    ] {
        let output = ProcessCommand::new(&executable)
            .current_dir(&root)
            .args(["--bind", "127.0.0.1:0", "--data-dir"])
            .arg(directory)
            .env("FINTWIND_DAEMON_TOKEN", Uuid::new_v4().to_string())
            .stdin(Stdio::null())
            .output()
            .expect("launch real daemon for fail-closed isolation case");
        assert!(
            !output.status.success(),
            "an invalid isolation path must fail"
        );
        assert!(
            output.stdout.is_empty(),
            "failed startup must not emit a ready envelope"
        );
        scenarios.push(serde_json::json!({"name": label, "passed": true, "readyAnnounced": false}));
    }
    assert!(
        !root.join("relative-isolation").exists(),
        "relative input must not create a data directory"
    );
    std::fs::write(
        root.join("summary.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "scenario": "invalid-daemon-data-directories", "passed": true,
            "publicChainVerified": false, "checks": scenarios,
        }))
        .unwrap(),
    )
    .expect("save redacted process black-box artifact");
    println!("[ARTIFACT] {}", root.join("summary.json").display());
}
