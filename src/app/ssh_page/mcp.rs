use super::*;
use fintwind_client::persistence::StateStore;
use fintwind_protocol::provider_session::McpConnectionState;

#[derive(Default)]
pub(super) struct RemoteMcp {
    workspace: Option<(PathBuf, PathBuf)>,
    signing_in: Option<String>,
    authorization_url: Option<String>,
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

    fn start_ssh_mcp_login(&mut self, name: String, cx: &mut Context<Self>) {
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
        let Some((binary, directory)) = state.workspace.clone() else {
            return;
        };
        let job = Uuid::new_v4();
        state.job = Some(job);
        state.signing_in = Some(name.clone());
        state.authorization_url = None;
        state.error = None;
        cx.spawn(async move |this, cx| {
            let (urls, events) = smol::channel::bounded::<String>(1);
            let work = cx.background_executor().spawn(async move {
                let runtime = tunnel::RUNTIME
                    .as_ref()
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                runtime.block_on(async {
                    let (session, _) =
                        tokio::time::timeout(Duration::from_secs(20), authenticate(&profile))
                            .await??;
                    tunnel::authenticate_mcp(
                        session,
                        binary.to_str().context("invalid OpenCode path")?,
                        directory.to_str().context("invalid workspace path")?,
                        &name,
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
                        app.ssh.mcp.authorization_url = Some(url);
                        cx.notify();
                    }
                });
            }
            let result = work.await;
            let _ = this.update(cx, |app, cx| {
                if app.ssh.mcp.job != Some(job) {
                    return;
                }
                app.ssh.mcp.job = None;
                app.ssh.mcp.authorization_url = None;
                match result {
                    Ok(()) => {
                        if let Some(name) = app.ssh.mcp.signing_in.take() {
                            app.show_success_toast(tr!("mcp.oauth_signed_in", name = name));
                        }
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
        let theme = Theme::current(cx);
        let selected = state.signing_in.as_deref() == Some(name);
        if let Some(url) = state.authorization_url.as_ref().filter(|_| selected) {
            let url = url.clone();
            return div().child(
                Button::new("ssh-mcp-reopen-login", tr!("mcp.oauth_sign_in"))
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        let phase = if selected {
            ssh_wait_phase(&state.loading, (), busy, cx)
        } else {
            LoadingPhase::Done
        };
        let focus = self.transcript_control_focus("ssh-mcp-login", cx);
        let server_name = name.to_owned();
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(ssh_wait_button(
                cx,
                theme,
                "ssh-mcp-login",
                &focus,
                tr!("mcp.oauth_sign_in"),
                ButtonVariant::Outline,
                state.workspace.is_some() && !busy,
                selected && busy,
                phase == LoadingPhase::Visible,
                move |app, cx| {
                    if app.commit_mcp_servers(cx) {
                        app.start_ssh_mcp_login(server_name.clone(), cx);
                    }
                },
            ))
            .when_some(state.error.as_ref().filter(|_| selected), |body, error| {
                body.child(
                    div()
                        .text_size(ui_px(12.0))
                        .text_color(theme.danger_text)
                        .child(error.clone()),
                )
            })
    }
}
