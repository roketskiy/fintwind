use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use russh::ChannelMsg;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio::time::timeout;

use russh::client::{Handle, Handler};

const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;

pub async fn authenticate_mcp<H: Handler + Sync + 'static>(
    session: Handle<H>,
    binary: &str,
    workspace: &str,
    name: &str,
    port: u16,
    mut authorization_url: impl FnMut(String) + Send,
) -> Result<()> {
    ensure!(
        port != 0,
        "specify the callback port configured for the MCP server"
    );
    ensure!(
        binary.starts_with('/') && workspace.starts_with('/'),
        "use absolute Linux paths"
    );
    ensure!(
        !name.is_empty() && !name.starts_with('-'),
        "invalid MCP name"
    );

    let listener = TcpListener::bind((LOOPBACK, port))
        .await
        .with_context(|| format!("cannot listen on local 127.0.0.1:{port}"))?;
    let session = Arc::new(session);
    let command = format!(
        "cd {} && exec /usr/bin/timeout -k 5s 350s {} mcp auth {}",
        shell_arg(workspace),
        shell_arg(binary),
        shell_arg(name)
    );
    let result = timeout(Duration::from_secs(360), async {
        let relay = relay_callbacks(session.clone(), listener, port);
        tokio::pin!(relay);
        tokio::select! {
            result = authenticate(&session, command, &mut authorization_url) => {
                result?;
                // The browser response may still be crossing the tunnel when the CLI exits.
                let _ = timeout(Duration::from_secs(1), &mut relay).await;
                Ok(())
            }
            result = &mut relay => result,
        }
    })
    .await;
    let _ = timeout(
        Duration::from_secs(2),
        session.disconnect(
            russh::Disconnect::ByApplication,
            "MCP authorization ended",
            "en",
        ),
    )
    .await;
    result.context("MCP authorization timed out")?
}

fn shell_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

async fn authenticate<H: Handler>(
    session: &Handle<H>,
    command: String,
    authorize: &mut (impl FnMut(String) + Send),
) -> Result<()> {
    let mut channel = session.channel_open_session().await?;
    channel.exec(true, command).await?;
    channel.eof().await?;
    let mut status = None;
    let mut buffers = [Vec::new(), Vec::new()];
    let mut sent = false;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => {
                receive_url(&mut buffers[0], &data, &mut sent, authorize)?;
            }
            ChannelMsg::ExtendedData { data, ext: 1 } => {
                receive_url(&mut buffers[1], &data, &mut sent, authorize)?;
            }
            ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
            _ => {}
        }
    }
    ensure!(
        status == Some(0),
        "remote OpenCode authorization failed (exit status: {status:?})"
    );
    Ok(())
}

fn receive_url(
    buffer: &mut Vec<u8>,
    bytes: &[u8],
    sent: &mut bool,
    authorize: &mut impl FnMut(String),
) -> Result<()> {
    if *sent {
        return Ok(());
    }
    ensure!(
        buffer.len() + bytes.len() <= 32768,
        "MCP authorization output too large"
    );
    buffer.extend_from_slice(bytes);
    if let Some(end) = buffer.iter().rposition(|byte| *byte == b'\n') {
        let output = String::from_utf8_lossy(&buffer[..=end]);
        for line in output.lines() {
            let Some((_, value)) = line.split_once("https://") else {
                continue;
            };
            let value = value
                .split(|c: char| c.is_whitespace() || c.is_control())
                .next()
                .unwrap_or_default();
            let url = format!("https://{value}");
            if url::Url::parse(&url).is_ok_and(|url| {
                url.has_host() && url.username().is_empty() && url.password().is_none()
            }) {
                *sent = true;
                authorize(url);
                break;
            }
        }
        buffer.drain(..=end);
    }
    Ok(())
}

async fn relay_callbacks<H: Handler + Sync + 'static>(
    session: Arc<Handle<H>>,
    listener: TcpListener,
    port: u16,
) -> Result<()> {
    let mut active = JoinSet::new();
    loop {
        tokio::select! {
            connection = listener.accept(), if active.len() < 8 => {
                let (mut browser, peer) = connection?;
                let session = session.clone();
                active.spawn(async move {
                    let relay = async {
                        let channel = session.channel_open_direct_tcpip(
                            "127.0.0.1", u32::from(port), "127.0.0.1", u32::from(peer.port()),
                        ).await?;
                        tokio::io::copy_bidirectional(&mut browser, &mut channel.into_stream()).await?;
                        Ok::<_, anyhow::Error>(())
                    };
                    let _ = timeout(Duration::from_secs(10), relay).await;
                });
            }
            _ = active.join_next(), if !active.is_empty() => {}
        }
    }
}
