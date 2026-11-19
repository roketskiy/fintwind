use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use russh::ChannelMsg;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio::time::timeout;

use russh::client::{Handle, Handler};
use url::{Host, Url};

pub async fn authenticate_mcp<H: Handler + Sync + 'static>(
    session: Handle<H>,
    binary: &str,
    workspace: &str,
    name: &str,
    mut authorization_url: impl FnMut(String) + Send,
) -> Result<()> {
    ensure!(
        binary.starts_with('/') && workspace.starts_with('/'),
        "use absolute Linux paths"
    );
    ensure!(
        !name.is_empty() && !name.starts_with('-'),
        "invalid MCP name"
    );

    let session = Arc::new(session);
    let command = format!(
        "cd {} && exec /usr/bin/timeout -k 5s 350s {} mcp auth {}",
        shell_arg(workspace),
        shell_arg(binary),
        shell_arg(name)
    );
    let result = timeout(Duration::from_secs(360), async {
        let (urls, mut pending) = tokio::sync::mpsc::channel(1);
        let mut publish = |url| {
            let _ = urls.try_send(url);
        };
        let auth = authenticate(&session, command, &mut publish);
        tokio::pin!(auth);
        let url = tokio::select! {
            result = &mut auth => return result,
            url = pending.recv() => url.context("OpenCode did not provide an authorization URL")?,
        };
        let redirect = url
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .context("The MCP authorization URL has no callback address")?
            .1;
        let callback = Url::parse(&redirect).context("Invalid MCP callback address")?;
        ensure!(
            callback.scheme() == "http"
                && callback.username().is_empty()
                && callback.password().is_none(),
            "Expected an HTTP loopback OAuth callback"
        );
        let host = match callback.host() {
            Some(Host::Domain("localhost")) => "127.0.0.1".to_owned(),
            Some(Host::Ipv4(ip)) if ip.is_loopback() => ip.to_string(),
            Some(Host::Ipv6(ip)) if ip.is_loopback() => ip.to_string(),
            _ => anyhow::bail!("Expected a loopback OAuth callback"),
        };
        let port = callback
            .port_or_known_default()
            .filter(|port| *port != 0)
            .context("Invalid MCP callback port")?;
        let listener = TcpListener::bind((host.as_str(), port))
            .await
            .with_context(|| format!("Cannot open local MCP callback port {port}"))?;
        authorization_url(url.into());
        let relay = relay_callbacks(session.clone(), listener, host, port);
        tokio::pin!(relay);
        tokio::select! {
            result = &mut auth => {
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
    authorize: &mut (impl FnMut(Url) + Send),
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
    authorize: &mut impl FnMut(Url),
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
            if let Ok(url) = Url::parse(&url)
                && url.has_host()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query_pairs().any(|(key, _)| key == "redirect_uri")
            {
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
    host: String,
    port: u16,
) -> Result<()> {
    let mut active = JoinSet::new();
    loop {
        tokio::select! {
            connection = listener.accept(), if active.len() < 8 => {
                let (mut browser, peer) = connection?;
                let session = session.clone();
                let host = host.clone();
                active.spawn(async move {
                    let relay = async {
                        let channel = session.channel_open_direct_tcpip(
                            host, u32::from(port), peer.ip().to_string(), u32::from(peer.port()),
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
