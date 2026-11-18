use super::*;
use fintwind_client::persistence::StateStore;
use fintwind_protocol::provider_session::McpConnectionState;

#[derive(Default)]
pub(super) struct RemoteMcp {
    workspace: Option<(PathBuf, PathBuf)>,
    editor: Option<(String, Entity<ComposerInput>)>,
    job: Option<Uuid>,
    loading: RefCell<Option<LoadingDelay<()>>>,
    error: Option<String>,
}

impl Fintwind {
    pub(in crate::app) fn load_ssh_mcp(&mut self, cx: &mut Context<Self>) {
        let Some(daemon) = self.settings_ssh_supervisor() else {
            return;
        };
        if self.ssh.mcp.job.is_some() {
            return;
        }
        let job = Uuid::new_v4();
        self.ssh.mcp.job = Some(job);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { tunnel::opencode_workspace(&daemon.client()) })
                .await;
            let _ = this.update(cx, |app, cx| {
                if app.ssh.mcp.job != Some(job) {
                    return;
                }
                app.ssh.mcp.job = None;
                match result {
                    Ok(workspace) => app.ssh.mcp.workspace = Some(workspace),
                    Err(error) => app.show_toast(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn start_ssh_mcp_login(&mut self, cx: &mut Context<Self>) {
        let Some(daemon) = self.settings_ssh_supervisor() else {
            return;
        };
        let Some(profile) = self
            .ssh
            .profiles
            .iter()
            .find(|p| Some(p.id) == self.settings_ssh_host())
            .cloned()
        else {
            return;
        };
        let state = &mut self.ssh.mcp;
        if state.job.is_some() {
            return;
        }
        let Some((name, port)) = &state.editor else {
            return;
        };
        let Ok(port) = port.read(cx).content().trim().parse::<u16>() else {
            return;
        };
        if port == 0 {
            return;
        }
        let name = name.clone();
        let Some((binary, directory)) = state.workspace.clone() else {
            return;
        };
        let job = Uuid::new_v4();
        state.job = Some(job);
        state.error = None;
        cx.spawn(async move |this, cx| {
            let (urls, events) = smol::channel::bounded::<String>(1);
            let work = cx.background_executor().spawn(async move {
                let runtime = tunnel::RUNTIME
                    .as_ref()
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                runtime.block_on(async {
                    let session =
                        tokio::time::timeout(Duration::from_secs(20), authenticate(&profile))
                            .await??
                            .map_err(|_| anyhow::anyhow!("SSH host fingerprint is not trusted"))?;
                    tunnel::authenticate_mcp(
                        session,
                        binary.to_str().context("invalid OpenCode path")?,
                        directory.to_str().context("invalid workspace path")?,
                        &name,
                        port,
                        |url| {
                            let _ = urls.try_send(url);
                        },
                    )
                    .await
                })?;
                drop(urls);
                let store = StateStore::remote(daemon);
                store.reload_opencode_server(binary.clone(), directory.clone())?;
                let rows = store.list_mcp_server_statuses(binary, directory)?;
                ensure!(
                    rows.iter()
                        .any(|row| row.name == name && row.status == McpConnectionState::Connected),
                    "{}",
                    tr!("mcp.ssh_not_confirmed")
                );
                Ok::<_, anyhow::Error>(())
            });
            if let Ok(url) = events.recv().await {
                let _ = this.update(cx, |app, cx| {
                    if app.ssh.mcp.job == Some(job) {
                        cx.open_url(&url);
                    }
                });
            }
            let result = work.await;
            let _ = this.update(cx, |app, cx| {
                if app.ssh.mcp.job != Some(job) {
                    return;
                }
                app.ssh.mcp.job = None;
                match result {
                    Ok(()) => {
                        app.ssh.mcp.editor = None;
                        app.refresh_mcp_statuses(cx);
                    }
                    Err(error) => app.ssh.mcp.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in crate::app) fn render_ssh_mcp_login(&self, name: &str, cx: &mut Context<Self>) -> Div {
        let state = &self.ssh.mcp;
        let busy = state.job.is_some();
        let mut body = div().flex().flex_col().gap(px(10.0));
        let server_name = name.to_owned();
        if state
            .editor
            .as_ref()
            .is_none_or(|(selected, _)| selected != name)
        {
            return body.child(
                Button::new("ssh-mcp-login-edit", tr!("mcp.oauth_sign_in"))
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Sm)
                    .disabled(busy)
                    .on_click(cx.listener(move |app, _, window, cx| {
                        if !app.commit_mcp_servers(cx) {
                            return;
                        }
                        let port = cx.new(|cx| {
                            ComposerInput::new(window, cx)
                                .search_field()
                                .placeholder(tr!("mcp.ssh_callback_port"))
                        });
                        cx.subscribe(&port, |_, _, _: &ComposerEvent, cx| cx.notify())
                            .detach();
                        app.ssh.mcp.error = None;
                        app.ssh.mcp.editor = Some((server_name.clone(), port));
                        cx.notify();
                    })),
            );
        }
        if let Some((name, port)) = &state.editor {
            let theme = Theme::current(cx);
            let phase = ssh_wait_phase(&state.loading, (), busy, cx);
            let focus = self.transcript_control_focus("ssh-mcp-login", cx);
            let valid = port
                .read(cx)
                .content()
                .trim()
                .parse::<u16>()
                .is_ok_and(|port| port != 0);
            body = body.child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .items_center()
                    .child(name.clone())
                    .child(TextField::new("ssh-mcp-port", port.clone()).w(px(180.0)))
                    .child(ssh_wait_button(
                        cx,
                        theme,
                        "ssh-mcp-login",
                        &focus,
                        tr!("mcp.oauth_sign_in"),
                        ButtonVariant::Primary,
                        valid && !busy && phase != LoadingPhase::Visible,
                        busy || phase == LoadingPhase::Visible,
                        phase == LoadingPhase::Visible,
                        |app, cx| app.start_ssh_mcp_login(cx),
                    )),
            );
            if let Some(error) = &state.error {
                body = body.child(
                    div()
                        .text_size(ui_px(12.0))
                        .text_color(theme.danger_text)
                        .child(error.clone()),
                );
            }
        }
        body
    }
}
