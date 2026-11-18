//! Opt-in black-box probe against an existing SSH server, not a product client.
//! Only the generated SFTP directory and its two files are removed by this probe.

use std::{future::Future, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use russh::{
    ChannelMsg, client,
    keys::{
        PrivateKeyWithHashAlg, PublicKeyOrCertificate, known_hosts::check_known_hosts_path,
        load_secret_key,
    },
};
use russh_sftp::client::SftpSession;
use serde::Serialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_secs(45);

mod daemon;
mod mcp_oauth;

#[derive(Clone)]
struct SshTarget {
    host: String,
    port: u16,
    user: String,
    key: PathBuf,
    known_hosts: PathBuf,
}

#[derive(Clone)]
struct Config {
    ssh: SshTarget,
    report: PathBuf,
    daemon: Option<daemon::Target>,
}

impl SshTarget {
    fn from_args(args: &[std::ffi::OsString]) -> Result<Self> {
        Ok(Self {
            host: args[0].to_string_lossy().into_owned(),
            port: args[1]
                .to_string_lossy()
                .parse()
                .context("invalid SSH port")?,
            user: args[2].to_string_lossy().into_owned(),
            key: args[3].clone().into(),
            known_hosts: args[4].clone().into(),
        })
    }
}

impl Config {
    fn from_args(args: &[std::ffi::OsString]) -> Result<Self> {
        ensure!(
            matches!(args.len(), 6 | 8 | 11),
            "usage: fintwind-ssh-probe HOST PORT USER PRIVATE_KEY KNOWN_HOSTS REPORT.json [DAEMON_PORT REMOTE_WORKSPACE [REMOTE_OPENCODE MODEL EXPECTED_TEXT]]"
        );
        Ok(Self {
            ssh: SshTarget::from_args(args)?,
            report: args[5].clone().into(),
            daemon: args
                .get(6)
                .map(|port| -> Result<_> {
                    Ok(daemon::Target {
                        port: port
                            .to_string_lossy()
                            .parse()
                            .context("invalid daemon port")?,
                        cwd: args[7].clone().into(),
                        agent: args.get(8).map(|binary| daemon::Agent {
                            binary: binary.clone().into(),
                            model: args[9].to_string_lossy().into_owned(),
                            expected: args[10].to_string_lossy().into_owned(),
                        }),
                    })
                })
                .transpose()?,
        })
    }
}

struct Handler(SshTarget);

impl client::Handler for Handler {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        Ok(check_known_hosts_path(
            &self.0.host,
            self.0.port,
            &key.public_key(),
            &self.0.known_hosts,
        )?)
    }
}

type Session = client::Handle<Handler>;

async fn connect(config: &SshTarget) -> Result<Session> {
    let mut session = client::connect(
        Arc::new(client::Config {
            nodelay: true,
            ..Default::default()
        }),
        (config.host.as_str(), config.port),
        Handler(config.clone()),
    )
    .await
    .context("SSH connection or known_hosts verification failed")?;
    let key = load_secret_key(&config.key, None).context("load private key")?;
    let hash = session.best_supported_rsa_hash().await?.flatten();
    let auth = session
        .authenticate_publickey(
            &config.user,
            PrivateKeyWithHashAlg::new(Arc::new(key), hash),
        )
        .await?;
    ensure!(auth.success(), "public-key authentication rejected");
    Ok(session)
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

#[derive(Serialize)]
struct Report {
    target: String,
    checks: Vec<Check>,
    all_passed: bool,
}

async fn check<F: Future<Output = Result<String>>>(
    checks: &mut Vec<Check>,
    name: &'static str,
    future: F,
) {
    let result = match timeout(DEADLINE, future).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "timed out after {} seconds",
            DEADLINE.as_secs()
        )),
    };
    let (passed, detail) = match result {
        Ok(detail) => (true, detail),
        Err(error) => (false, format!("{error:#}")),
    };
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    checks.push(Check {
        name,
        passed,
        detail,
    });
}

async fn command(session: &Session) -> Result<String> {
    let mut channel = session.channel_open_session().await?;
    // The SSH server starts its normal login shell; the actual probe uses a
    // fixed environment, a fixed command, and a server-side time limit.
    channel.exec(true, "exec /usr/bin/timeout 10s /usr/bin/env -i PATH=/usr/bin:/bin /bin/sh -c 'printf out-marker; printf err-marker >&2; exit 7'").await?;
    let (mut stdout, mut stderr, mut exit) = (Vec::new(), Vec::new(), None);
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, ext: 1 } => stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
            _ => {}
        }
    }
    ensure!(
        stdout == b"out-marker" && stderr == b"err-marker" && exit == Some(7),
        "unexpected command output or exit status: stdout={stdout:?}, stderr={stderr:?}, exit={exit:?}"
    );
    Ok("stdout, stderr and non-zero exit status received".into())
}

async fn pty(session: &Session) -> Result<String> {
    let mut channel = session.channel_open_session().await?;
    channel
        .request_pty(true, "xterm-256color", 80, 24, 0, 0, &[])
        .await?;
    channel.exec(true, "exec /usr/bin/timeout 10s /usr/bin/env -i PATH=/usr/bin:/bin TERM=xterm-256color /bin/sh").await?;
    channel.window_change(101, 37, 0, 0).await?;
    channel
        .data(&b"printf 'pty-%s\\n' marker; stty size; exit\n"[..])
        .await?;
    let mut output = Vec::new();
    let mut exit = None;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => output.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
            _ => {}
        }
    }
    let output = String::from_utf8_lossy(&output);
    ensure!(
        output.contains("pty-marker")
            && output.lines().any(|line| line.trim() == "37 101")
            && exit == Some(0),
        "PTY output, resize or exit mismatch: {output:?}, exit={exit:?}"
    );
    Ok("interactive input/output and resize to 101x37".into())
}

async fn sftp(session: &Session) -> Result<String> {
    let channel = session.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = SftpSession::new(channel.into_stream()).await?;
    let directory = format!(".fintwind-ssh-probe-{}", Uuid::new_v4());
    timeout(Duration::from_secs(10), sftp.create_dir(&directory))
        .await
        .with_context(|| format!("directory creation timed out; inspect {directory} manually"))?
        .context("create exclusive probe directory")?;
    let first = format!("{directory}/sample.txt");
    let second = format!("{directory}/renamed.txt");
    let result: Result<()> = match timeout(Duration::from_secs(10), async {
        let content = b"Fintwind SSH probe\n\x00binary\n";
        let mut file = sftp.create(&first).await.context("create probe file")?;
        file.write_all(content).await.context("write probe file")?;
        file.close().await.context("close probe file")?;
        ensure!(
            sftp.read(&first).await.context("read uploaded file")? == content,
            "uploaded content did not match"
        );
        sftp.rename(&first, &second)
            .await
            .context("rename probe file")?;
        let mut entries = sftp
            .read_dir(&directory)
            .await
            .context("list probe directory")?;
        ensure!(
            entries.any(|entry| entry.file_name() == "renamed.txt"),
            "renamed file missing from directory listing"
        );
        ensure!(
            sftp.read(&second).await.context("read renamed file")? == content,
            "renamed file content did not match"
        );
        Ok(())
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("SFTP file operations timed out")),
    };
    // Explicitly remove only names this run created, then the empty directory.
    let cleanup = async {
        for file in [&first, &second] {
            // Either name can be absent after a successful or interrupted rename.
            let _ = timeout(Duration::from_secs(5), sftp.remove_file(file)).await;
        }
        timeout(Duration::from_secs(5), sftp.remove_dir(&directory))
            .await
            .context("cleanup timed out")?
            .context("remove owned probe directory")?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = cleanup {
        bail!("probe result: {result:?}; cleanup failed for {directory}: {error:#}");
    }
    result?;
    Ok("upload, download, rename, list and owned-file cleanup".into())
}

async fn forward(session: &Session) -> Result<String> {
    // Read the banner of the existing server through direct-tcpip; no new
    // remote service is started and no authentication is attempted here.
    let channel = session
        .channel_open_direct_tcpip("127.0.0.1", 22, "127.0.0.1", 0)
        .await?;
    let mut stream = channel.into_stream();
    let mut banner = Vec::new();
    loop {
        let byte = stream.read_u8().await?;
        banner.push(byte);
        ensure!(banner.len() <= 255, "invalid SSH banner");
        if byte == b'\n' {
            break;
        }
    }
    ensure!(
        banner.starts_with(b"SSH-2.0-"),
        "unexpected forwarded banner: {banner:?}"
    );
    Ok("direct-tcpip reached the remote loopback SSH service".into())
}

async fn run(config: &Config, checks: &mut Vec<Check>) {
    let session = match timeout(DEADLINE, connect(&config.ssh)).await {
        Ok(Ok(session)) => {
            checks.push(Check {
                name: "connect_public_key_known_hosts",
                passed: true,
                detail: "known host key and public-key authentication accepted".into(),
            });
            println!("PASS connect_public_key_known_hosts");
            session
        }
        other => {
            let detail = match other {
                Ok(Err(error)) => format!("{error:#}"),
                Err(_) => "connection timed out".into(),
                _ => unreachable!(),
            };
            println!("FAIL connect_public_key_known_hosts: {detail}");
            checks.push(Check {
                name: "connect_public_key_known_hosts",
                passed: false,
                detail,
            });
            return;
        }
    };
    if let Some(target) = &config.daemon {
        daemon::run(session, &config.ssh, target, checks).await;
        return;
    }
    check(checks, "remote_exec", command(&session)).await;
    check(checks, "interactive_pty_resize", pty(&session)).await;
    check(checks, "sftp_files", sftp(&session)).await;
    check(checks, "direct_tcpip", forward(&session)).await;
    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "probe complete", "en")
        .await;
    check(checks, "reconnect", async {
        let session = connect(&config.ssh).await?;
        let result = command(&session).await?;
        session
            .disconnect(russh::Disconnect::ByApplication, "probe complete", "en")
            .await?;
        Ok(format!("new authenticated connection: {result}"))
    })
    .await;
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(args) = args.strip_prefix(&["--mcp-oauth".into()]) {
        return mcp_oauth::run(args).await;
    }
    let config = Config::from_args(&args)?;
    let mut checks = Vec::new();
    run(&config, &mut checks).await;
    let report = Report {
        target: format!(
            "{}@{}:{}",
            config.ssh.user, config.ssh.host, config.ssh.port
        ),
        all_passed: checks.iter().all(|check| check.passed),
        checks,
    };
    if let Some(parent) = config.report.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&config.report, serde_json::to_vec_pretty(&report)?)?;
    println!("report: {}", config.report.display());
    ensure!(report.all_passed, "one or more SSH checks failed");
    Ok(())
}
