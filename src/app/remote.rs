use super::*;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    primitives::IconName,
    theme::ControlSize,
};
use image::{Frame, Rgba, RgbaImage};
use qrcode::{Color as QrColor, QrCode};
use serde_json::{Value, json};

struct RemoteUpload {
    session_id: Uuid,
    name: String,
    encoded: String,
    touched: Instant,
    stored: Option<MessageAttachment>,
    finalizing: bool,
}

#[derive(Default)]
pub(super) struct RemoteUploads(std::collections::HashMap<Uuid, RemoteUpload>);

impl RemoteUploads {
    fn append(
        &mut self,
        session_id: Uuid,
        id: Uuid,
        name: &str,
        offset: usize,
        data: &str,
        complete: bool,
    ) -> Result<Option<String>, String> {
        self.0
            .retain(|_, upload| upload.touched.elapsed() < Duration::from_secs(600));
        if name.is_empty()
            || name.len() > 128
            || name.contains(['/', '\\', ':'])
            || name.chars().any(char::is_control)
            || name == "."
            || name == ".."
            || data.len() > 64 * 1024
            || data.len() % 4 != 0
            || (!complete && data.contains('='))
            || !data
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
        {
            return Err("附件参数无效。".into());
        }
        let total: usize = self.0.values().map(|upload| upload.encoded.len()).sum();
        if total.saturating_add(data.len()) > 11_184_832
            || (!self.0.contains_key(&id) && self.0.len() >= 8)
        {
            return Err("附件总量超限，请重新配对后重试。".into());
        }
        if !self.0.contains_key(&id) && offset != 0 {
            return Err("附件上传已过期，请重新选择。".into());
        }
        let upload = self.0.entry(id).or_insert_with(|| RemoteUpload {
            session_id,
            name: name.to_owned(),
            encoded: String::new(),
            touched: Instant::now(),
            stored: None,
            finalizing: false,
        });
        if upload.session_id != session_id
            || upload.name != name
            || upload.encoded.len() != offset
            || upload.stored.is_some()
            || upload.finalizing
            || offset.saturating_add(data.len()) > 2_796_204
        {
            return Err("附件上传状态无效，请重新选择。".into());
        }
        upload.encoded.push_str(data);
        upload.touched = Instant::now();
        upload.finalizing = complete;
        Ok(complete.then(|| upload.encoded.clone()))
    }

    fn resolve(&self, session_id: Uuid, ids: &[Uuid]) -> Result<Vec<MessageAttachment>, String> {
        let mut unique = std::collections::HashSet::new();
        ids.iter()
            .map(|id| {
                let upload = self
                    .0
                    .get(id)
                    .filter(|upload| {
                        upload.session_id == session_id
                            && upload.touched.elapsed() < Duration::from_secs(600)
                            && unique.insert(*id)
                    })
                    .ok_or("附件上传已过期，请重新选择。")?;
                upload
                    .stored
                    .clone()
                    .ok_or_else(|| "附件尚未上传完成。".into())
            })
            .collect()
    }
}

const REMOTE_WINDOW_SECS: u64 = 72 * 60 * 60;
const REMOTE_SESSION_LIMIT: usize = 200;
const REMOTE_MESSAGE_LIMIT: usize = 100;
const REMOTE_RESPONSE_LIMIT: usize = 512 * 1024;
const REMOTE_MESSAGE_TEXT_LIMIT: usize = 16 * 1024;
const REMOTE_QR_SIZE: f32 = 224.0;
const REMOTE_WORKSPACE_REFRESH: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct RemoteWorkspaceChanges {
    entries: HashMap<PathBuf, (Instant, Option<(u64, u64)>)>,
    pending: std::collections::HashSet<PathBuf>,
}

#[derive(Clone, Debug)]
pub(super) enum RemoteUiStatus {
    Off,
    Connecting,
    Waiting,
    Connected,
    Failed,
}

impl RemoteUiStatus {
    fn label(&self) -> String {
        match self {
            Self::Off => tr!("remote.status_off"),
            Self::Connecting => tr!("remote.status_connecting"),
            Self::Waiting => tr!("remote.status_waiting"),
            Self::Connected => tr!("remote.status_connected"),
            Self::Failed => tr!("remote.status_failed"),
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Self::Off | Self::Waiting => "icons/laptop.svg",
            Self::Connecting => "icons/loader-circle.svg",
            Self::Connected => "icons/check.svg",
            Self::Failed => "icons/alert.svg",
        }
    }

    fn tint(&self, theme: Theme) -> Hsla {
        match self {
            Self::Connected => theme.success,
            Self::Failed => theme.warning,
            Self::Off => theme.text_tertiary,
            Self::Connecting | Self::Waiting => theme.text_secondary,
        }
    }

    fn has_pairing(&self) -> bool {
        matches!(self, Self::Waiting | Self::Connected)
    }
}

fn session_updated_at(session: &AgentSession) -> u64 {
    session.updated_at
}

fn session_is_recent(session: &AgentSession, cutoff: u64) -> bool {
    session_updated_at(session) >= cutoff
}

fn bounded_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn bounded_tail_text(text: &str, max_bytes: usize) -> String {
    let mut start = text.len().saturating_sub(max_bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

fn qr_image(url: &str) -> anyhow::Result<Arc<gpui::RenderImage>> {
    let code = QrCode::new(url.as_bytes())?;
    let modules = code.width();
    let quiet_zone = 4usize;
    // Match the rendered size so the bitmap is sampled near 1:1, not downscaled
    // from a fixed 5x source.
    let rendered_modules = modules + quiet_zone * 2;
    let scale = ((REMOTE_QR_SIZE as usize).div_ceil(rendered_modules)).max(2);
    let side = rendered_modules * scale;
    let mut bitmap = RgbaImage::from_pixel(side as u32, side as u32, Rgba([255, 255, 255, 255]));
    let colors = code.to_colors();
    for y in 0..modules {
        for x in 0..modules {
            if colors[y * modules + x] == QrColor::Dark {
                let left = ((x + quiet_zone) * scale) as u32;
                let top = ((y + quiet_zone) * scale) as u32;
                for py in top..top + scale as u32 {
                    for px in left..left + scale as u32 {
                        bitmap.put_pixel(px, py, Rgba([0, 0, 0, 255]));
                    }
                }
            }
        }
    }
    Ok(Arc::new(gpui::RenderImage::new(vec![Frame::new(bitmap)])))
}

impl Fintwind {
    pub(super) fn render_remote_control(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let status = &self.remote_status;
        let enabled = self.remote_control.is_some() || self.remote_starting;

        let title_row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.0))
            .child(
                div()
                    .min_w_0()
                    .text_size(ui_px(18.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("remote.title")),
            )
            .child(
                toggle_switch(
                    "remote-enabled",
                    enabled,
                    false,
                    theme,
                    window,
                    cx,
                    move |this: &mut Self, _, cx| {
                        if this.remote_control.is_some() || this.remote_starting {
                            this.stop_remote_control();
                        } else {
                            this.start_remote_control(cx);
                        }
                        cx.notify();
                    },
                )
                .accessibility_label(tr!("remote.title")),
            );

        let status_icon = status.icon();
        let status_tint = status.tint(theme);
        let connected = matches!(status, RemoteUiStatus::Connected);

        let status_row = div()
            .px(px(20.0))
            .py(px(16.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .size(px(32.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.overlay)
                    .child(icon(status_icon, 16.0, status_tint)),
            )
            .child(
                div()
                    .min_w_0()
                    .text_size(ui_px(13.5))
                    .when(connected, |text| {
                        text.font_weight(FontWeight::MEDIUM).text_color(theme.text)
                    })
                    .when(!connected, |text| text.text_color(theme.text_secondary))
                    .child(status.label()),
            );

        let idle_state = div()
            .px(px(28.0))
            .py(px(36.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(14.0))
            .child(
                div()
                    .size(px(64.0))
                    .rounded(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.overlay)
                    .child(icon(status_icon, 30.0, status_tint)),
            )
            .child(
                div()
                    .text_size(ui_px(13.5))
                    .text_color(theme.text_secondary)
                    .child(status.label()),
            );

        let qr_panel = if let Some(qr) = self.remote_pairing_qr.clone() {
            div()
                .p(px(12.0))
                .rounded(px(14.0))
                .border_1()
                .border_color(theme.border)
                .bg(gpui::white())
                .child(img(qr).size(px(REMOTE_QR_SIZE)))
        } else {
            div()
                .p(px(12.0))
                .rounded(px(14.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.inset)
                .child(
                    div()
                        .size(px(REMOTE_QR_SIZE))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon("icons/laptop.svg", 28.0, theme.text_ghost)),
                )
        };
        let qr_section = div()
            .px(px(20.0))
            .py(px(20.0))
            .flex()
            .justify_center()
            .child(qr_panel);

        let copy_button = self.remote_pairing_url.as_ref().map(|_| {
            remote_button(
                "remote-copy-link",
                tr!("remote.copy_link"),
                ButtonVariant::Outline,
                IconName::Copy,
                false,
                cx,
                |this, cx| {
                    if let Some(url) = this.remote_pairing_url.clone() {
                        cx.write_to_clipboard(ClipboardItem::new_string(url));
                        this.show_success_toast(tr!("remote.copied"));
                    }
                },
            )
        });
        let actions = div()
            .px(px(20.0))
            .py(px(14.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .when_some(copy_button, |row, button| row.child(button))
            .child(remote_button(
                "remote-refresh",
                tr!("remote.refresh"),
                ButtonVariant::Secondary,
                IconName::RotateCw,
                self.remote_starting,
                cx,
                |this, cx| {
                    this.stop_remote_control();
                    this.start_remote_control(cx);
                    cx.notify();
                },
            ));

        let separator = || {
            div()
                .mx(px(20.0))
                .h(px(1.0))
                .bg(theme.border)
                .into_any_element()
        };
        let mut body = Vec::new();
        if status.has_pairing() {
            body.push(status_row.into_any_element());
            body.push(separator());
            if connected {
                body.push(actions.into_any_element());
                body.push(separator());
                body.push(qr_section.into_any_element());
            } else {
                body.push(qr_section.into_any_element());
                body.push(separator());
                body.push(actions.into_any_element());
            }
        } else {
            body.push(idle_state.into_any_element());
        }
        if let Some(error) = self.remote_error.as_ref() {
            body.push(
                div()
                    .px(px(20.0))
                    .pb(px(16.0))
                    .text_size(ui_px(12.5))
                    .line_height(ui_px(18.0))
                    .text_color(theme.warning_text)
                    .child(bounded_text(error, 512))
                    .into_any_element(),
            );
        }

        div()
            .id("remote-settings")
            .size_full()
            .overflow_y_scroll()
            .px(px(32.0))
            .pt(px(28.0))
            .pb(px(48.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(560.0))
                    .mx_auto()
                    .flex()
                    .flex_col()
                    .gap(px(20.0))
                    .child(title_row)
                    .child(
                        div()
                            .w_full()
                            .rounded(px(13.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.raised)
                            .shadow_xs()
                            .overflow_hidden()
                            .children(body),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn start_remote_control(&mut self, cx: &mut Context<Self>) {
        if self.remote_control.is_some() || self.remote_starting {
            return;
        }
        self.remote_generation = self.remote_generation.wrapping_add(1);
        let generation = self.remote_generation;
        self.remote_project_ids = self
            .state
            .projects
            .iter()
            .map(|project| project.id)
            .collect();
        self.remote_session_hydrations.clear();
        self.remote_pairing_url = None;
        self.remote_pairing_qr = None;
        self.remote_error = None;
        self.remote_starting = true;
        self.remote_status = RemoteUiStatus::Connecting;
        cx.spawn(async move |this, cx| {
            let started = cx
                .background_executor()
                .spawn(async move {
                    let config = fintwind_remote::RelayConfig::load_default()?;
                    let (control, events) = fintwind_remote::RemoteControl::start(config)?;
                    let pairing_url = control.pairing_url().to_owned();
                    Ok::<_, anyhow::Error>((control, events, pairing_url))
                })
                .await;
            let (control, events, pairing_url) = match started {
                Ok(started) => started,
                Err(_) => {
                    let _ = this.update(cx, |app, cx| {
                        if app.remote_generation == generation {
                            let message = tr!("remote.start_failed");
                            app.remote_starting = false;
                            app.remote_status = RemoteUiStatus::Failed;
                            app.remote_error = Some(message);
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let accepted = this.update(cx, |app, cx| {
                if app.remote_generation != generation || !app.remote_starting {
                    control.stop();
                    return false;
                }
                app.remote_control = Some(control);
                app.remote_pairing_url = Some(pairing_url.clone());
                app.remote_status = RemoteUiStatus::Waiting;
                app.remote_starting = false;
                cx.notify();
                true
            });
            if !accepted.unwrap_or(false) {
                return;
            }

            let qr = cx
                .background_executor()
                .spawn(async move { qr_image(&pairing_url) })
                .await;
            if let Ok(qr) = qr {
                let _ = this.update(cx, |app, cx| {
                    if app.remote_generation == generation {
                        app.remote_pairing_qr = Some(qr);
                        cx.notify();
                    }
                });
            }

            while let Ok(event) = events.recv().await {
                match event {
                    fintwind_remote::RemoteEvent::Status(status) => {
                        let _ = this.update(cx, |app, cx| {
                            if app.remote_generation == generation {
                                app.remote_status = match status {
                                    fintwind_remote::RemoteStatus::Connecting => {
                                        app.remote_error = None;
                                        RemoteUiStatus::Connecting
                                    }
                                    fintwind_remote::RemoteStatus::Waiting => {
                                        app.remote_error = None;
                                        RemoteUiStatus::Waiting
                                    }
                                    fintwind_remote::RemoteStatus::Connected => {
                                        app.remote_error = None;
                                        RemoteUiStatus::Connected
                                    }
                                    fintwind_remote::RemoteStatus::Failed(reason) => {
                                        app.remote_error = Some(reason);
                                        RemoteUiStatus::Failed
                                    }
                                };
                                cx.notify();
                            }
                        });
                    }
                    fintwind_remote::RemoteEvent::WatchSession { session_id, snapshots } => {
                        let _ = this.update(cx, |_, cx| {
                            let (changed, changes) = smol::channel::bounded(1);
                            let _ = changed.try_send(());
                            let entity = cx.entity();
                            let observation = cx.observe(&entity, move |_, _, _| {
                                let _ = changed.try_send(());
                            });
                            cx.spawn(async move |this, cx| {
                                let _observation = observation;
                                let mut clock = false;
                                let mut workspace_check = Instant::now();
                                loop {
                                    if snapshots.is_closed() { break; }
                                    let notified = smol::future::or(
                                        async { changes.recv().await.is_ok() },
                                        async { smol::Timer::after(Duration::from_secs(1)).await; false },
                                    ).await;
                                    if !notified && !clock && workspace_check.elapsed() < REMOTE_WORKSPACE_REFRESH { continue; }
                                    // Fold notifications, never the provider's individual chunks.
                                    smol::Timer::after(Duration::from_millis(200)).await;
                                    while changes.try_recv().is_ok() {}
                                    if snapshots.is_closed() { break; }
                                    if workspace_check.elapsed() >= REMOTE_WORKSPACE_REFRESH { workspace_check = Instant::now(); }
                                    let result = this.update(cx, |app, cx| {
                                        if app.remote_generation != generation { return Err("远控已关闭。".to_owned()); }
                                        let index = app.remote_session_index(session_id)?;
                                        app.refresh_remote_workspace_changes(index, cx);
                                        Ok(app.remote_session_projection(index, true))
                                    });
                                    let value = match result {
                                        Ok(value) => value,
                                        Err(_) => break,
                                    };
                                    clock = value.as_ref().ok().and_then(|value| value["goal"]["status"].as_str()).is_some_and(|status| status == "active");
                                    let failed = value.is_err();
                                    if snapshots.send(value).await.is_err() || failed { break; }
                                }
                            }).detach();
                        });
                    }
                    fintwind_remote::RemoteEvent::Request(request) => {
                        let _ = this.update(cx, |_, cx| {
                        cx.spawn(async move |this, cx| {
                        if let fintwind_remote::RemoteCommand::GetComposer { session_id } = &request.command {
                            let context = this.update(cx, |app, _| {
                                if app.remote_generation != generation { return Err("远控已关闭。".to_owned()); }
                                let index = app.remote_session_index(*session_id)?;
                                let directory = app.workspace_path_for_session(&app.state.sessions[index]).ok_or("工作区不可用。")?.to_path_buf();
                                let cached = (app.provider_probe_directory.as_ref() == Some(&directory)).then(|| app.provider_probe().cloned()).flatten();
                                Ok((app.daemon.clone(), directory, cached, app.provider_binary_generation))
                            }).unwrap_or_else(|_| Err("桌面窗口已关闭。".into()));
                            let response = match context {
                                Err(error) => Err(error),
                                Ok((daemon, directory, cached, binary_generation)) => {
                                    let probe = match cached {
                                        Some(probe) => Ok(probe),
                                        None => {
                                            let location = directory.clone();
                                            cx.background_executor().spawn(async move {
                                                match daemon.client().request(Uuid::nil(), Uuid::nil(), fintwind_client::Command::ProbeProvider {
                                                    binary_override: None, directory: Some(location), discover_models: true, probe_version: false,
                                                }) {
                                                    Ok(fintwind_client::ResponsePayload::ProviderProbe { probe, .. }) => Ok(probe),
                                                    _ => Err("无法加载此工作区的模型。".to_owned()),
                                                }
                                            }).await
                                        }
                                    };
                                    match probe {
                                        Err(error) => Err(error),
                                        Ok(probe) => {
                                            let result = this.update(cx, |app, _| {
                                                if app.remote_generation != generation { return Err("远控已关闭。".to_owned()); }
                                                if app.provider_binary_generation != binary_generation { return Err("模型目录已更新，请重试。".into()); }
                                                let index = app.remote_session_index(*session_id)?;
                                                if app.workspace_path_for_session(&app.state.sessions[index]) != Some(directory.as_path()) { return Err("工作区已改变。".into()); }
                                                if app.remote_model_catalogs.len() >= 8 && !app.remote_model_catalogs.contains_key(&directory) { app.remote_model_catalogs.clear(); }
                                                app.remote_model_catalogs.insert(directory.clone(), (Instant::now(), probe.clone()));
                                                if app.remote_composer_catalogs.len() >= 8 { app.remote_composer_catalogs.clear(); }
                                                app.remote_composer_catalogs.insert(*session_id, (directory.clone(), probe.clone(), Instant::now(), app.provider_binary_generation));
                                                Ok(app.remote_composer_snapshot(index, &probe))
                                            }).unwrap_or_else(|_| Err("桌面窗口已关闭。".into()));
                                            result
                                        }
                                    }
                                }
                            };
                            let _ = request.respond.send(response).await;
                            return;
                        }
                        if let fintwind_remote::RemoteCommand::UploadChunk { session_id, upload_id, name, offset, data_base64, complete } = &request.command {
                            let allowed = this.update(cx, |app, _| {
                                if app.remote_generation != generation { return Err("远控已关闭。".to_owned()); }
                                app.remote_session_index(*session_id)?;
                                let encoded = app.remote_uploads.append(*session_id, *upload_id, name, *offset, data_base64, *complete)?;
                                Ok((app.daemon.clone(), encoded))
                            }).unwrap_or_else(|_| Err("桌面窗口已关闭。".into()));
                            let response = match allowed {
                                Err(error) => Err(error),
                                Ok((daemon, encoded)) => {
                                        let mut result = Ok(json!({"accepted": true, "offset": offset + data_base64.len()}));
                                        if let Some(encoded) = encoded {
                                            let name = name.clone();
                                            let imported = cx.background_executor().spawn(async move {
                                                let bytes = STANDARD.decode(&encoded).map_err(|_| "附件编码无效。".to_owned())?;
                                                if bytes.is_empty() || bytes.len() > 2 * 1024 * 1024 { return Err("单个附件不能超过 2 MiB。".to_owned()); }
                                                let response = daemon.client().request(Uuid::nil(), Uuid::nil(), fintwind_client::Command::ImportAttachment {
                                                    name, upload: fintwind_protocol::attachments::AttachmentUpload::File { data_base64: encoded }
                                                }).map_err(|_| "无法保存附件。".to_owned())?;
                                                let fintwind_client::ResponsePayload::AttachmentStored { attachment } = response else { return Err("附件保存响应无效。".to_owned()); };
                                                Ok(attachment)
                                            }).await;
                                            result = this.update(cx, |app, _| {
                                                if app.remote_generation != generation { return Err("远控已关闭。".to_owned()); }
                                                app.remote_session_index(*session_id)?;
                                                match imported {
                                                Ok(stored) => {
                                                    if let Some(upload) = app.remote_uploads.0.get_mut(upload_id) {
                                                        let extension = std::path::Path::new(&stored.name).extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase();
                                                        upload.stored = Some(MessageAttachment { path: stored.path, mention: stored.name.clone(), name: stored.name, is_dir: false,
                                                            is_image: matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif"), blob_reference: Some(stored.reference) });
                                                    } else { return Err("附件上传已过期。".into()); }
                                                    Ok(json!({"accepted": true, "offset": offset + data_base64.len()}))
                                                }
                                                Err(error) => { app.remote_uploads.0.remove(upload_id); Err(error) }
                                            }}).unwrap_or_else(|_| Err("桌面窗口已关闭。".into()));
                                        }
                                        result
                                    }
                            };
                            let _ = request.respond.send(response).await;
                            return;
                        }
                        let attachment_ids = match &request.command {
                            fintwind_remote::RemoteCommand::SendPrompt { attachment_ids, .. } => attachment_ids.clone(),
                            _ => Vec::new(),
                        };
                        let response = this
                            .update(cx, |app, cx| {
                                if app.remote_generation != generation {
                                    return Err("Remote control has been stopped".to_owned());
                                }
                                let attachments = match &request.command {
                                    fintwind_remote::RemoteCommand::SendPrompt { session_id, attachment_ids, .. } => app.remote_uploads.resolve(*session_id, attachment_ids)?,
                                    _ => Vec::new(),
                                };
                                let catalog = match &request.command {
                                    fintwind_remote::RemoteCommand::ConfigureSession { session_id, .. } => app.remote_composer_catalogs.get(session_id).cloned(),
                                    _ => None,
                                };
                                let result = app.dispatch_remote_command(request.command, attachments, catalog.as_ref(), cx);
                                if result.is_ok() { for id in attachment_ids { app.remote_uploads.0.remove(&id); } }
                                result
                            })
                            .unwrap_or_else(|_| {
                                Err("Desktop window is no longer available".to_owned())
                            });
                        let sender = request.respond;
                        cx.background_executor()
                            .spawn(async move {
                                let _ = sender.send(response).await;
                            })
                            .detach();
                        }).detach();
                        });
                    }
                }
            }
            let _ = this.update(cx, |app, cx| {
                if app.remote_generation == generation {
                    let last_error = app.remote_error.clone();
                    app.stop_remote_control();
                    app.remote_status = RemoteUiStatus::Failed;
                    app.remote_error = last_error.or_else(|| Some(tr!("remote.connection_failed")));
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn stop_remote_control(&mut self) {
        self.remote_generation = self.remote_generation.wrapping_add(1);
        self.remote_starting = false;
        if let Some(control) = self.remote_control.take() {
            control.stop();
        }
        self.remote_project_ids.clear();
        self.remote_uploads.0.clear();
        self.remote_composer_catalogs.clear();
        self.remote_model_catalogs.clear();
        self.remote_workspace_changes = RemoteWorkspaceChanges::default();
        self.remote_pairing_url = None;
        self.remote_pairing_qr = None;
        self.remote_error = None;
        self.remote_status = RemoteUiStatus::Off;
    }

    pub(super) fn dispatch_remote_command(
        &mut self,
        command: fintwind_remote::RemoteCommand,
        attachments: Vec<MessageAttachment>,
        catalog: Option<&(PathBuf, ProviderProbe, Instant, u64)>,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        match command {
            fintwind_remote::RemoteCommand::WatchSession { .. } => {
                Err("Subscription must be handled by the encrypted transport".to_owned())
            }
            fintwind_remote::RemoteCommand::ListWorkspaces => {
                let workspaces = self
                    .state
                    .projects
                    .iter()
                    .filter(|project| self.remote_project_ids.contains(&project.id))
                    .take(REMOTE_SESSION_LIMIT)
                    .map(|project| json!({"id": project.id, "name": bounded_text(&project.display_name(), 256)}))
                    .collect::<Vec<_>>();
                let selected = self.state.selected_project.filter(|id| {
                    self.remote_project_ids.contains(id)
                        && self.state.projects.iter().any(|project| project.id == *id)
                });
                Ok(json!({"workspaces": workspaces, "selectedProjectId": selected}))
            }
            fintwind_remote::RemoteCommand::ListSessions { project_id } => {
                self.remote_check_project(project_id)?;
                let now = unix_time();
                let cutoff = now.saturating_sub(REMOTE_WINDOW_SECS);
                let mut sessions = self
                    .state
                    .sessions
                    .iter()
                    .filter(|session| {
                        session.project_id == project_id && session_is_recent(session, cutoff)
                    })
                    .collect::<Vec<_>>();
                sessions
                    .sort_unstable_by_key(|session| std::cmp::Reverse(session_updated_at(session)));
                sessions.truncate(REMOTE_SESSION_LIMIT);
                let sessions = sessions
                    .into_iter()
                    .map(|session| {
                        json!({
                            "id": session.id,
                            "projectId": session.project_id,
                            "title": bounded_text(&session.display_title(), 256),
                            "status": serde_json::to_value(session.status).unwrap_or(Value::Null),
                            "updatedAt": session_updated_at(session),
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(json!({"sessions": sessions, "cutoff": cutoff}))
            }
            fintwind_remote::RemoteCommand::GetSession { session_id } => {
                let index = self.remote_session_index(session_id)?;
                self.refresh_remote_workspace_changes(index, cx);
                if !self.state.sessions[index].detail_loaded {
                    if !self.runtimes.contains_key(&session_id)
                        || !self.state.sessions[index].status.is_busy()
                    {
                        self.hydrate_remote_session(session_id, cx);
                    }
                }
                Ok(self.remote_session_snapshot(index))
            }
            fintwind_remote::RemoteCommand::GetComposer { .. } => {
                Err("请重新打开会话选项。".into())
            }
            fintwind_remote::RemoteCommand::ConfigureSession {
                session_id,
                model,
                interaction_mode,
                reasoning_effort,
                runtime_mode,
            } => {
                let index = self.remote_session_index(session_id)?;
                let (directory, probe, at, binary_generation) =
                    catalog.ok_or("请重新打开会话选项。")?;
                if at.elapsed() > Duration::from_secs(30)
                    || *binary_generation != self.provider_binary_generation
                    || (self.provider_probe_directory.as_ref() == Some(directory)
                        && self
                            .provider_probe()
                            .is_none_or(|current| current.models != probe.models))
                {
                    return Err("模型选项已更新，请重新打开选择器。".into());
                }
                if self.workspace_path_for_session(&self.state.sessions[index])
                    != Some(directory.as_path())
                {
                    return Err("工作区已改变，请重新打开会话选项。".into());
                }
                if !self.state.sessions[index].detail_loaded
                    || !self.state.sessions[index].can_choose_model()
                    || self.submission_preparations.contains(&session_id)
                    || self.response_fork_preparations.contains_key(&session_id)
                {
                    return Err("请等待当前操作结束后再修改会话选项。".into());
                }
                let current_model = self.state.sessions[index]
                    .model
                    .clone()
                    .or_else(|| probe.preferred_model().map(|model| model.id.clone()));
                let selected_model = model.as_deref().or(current_model.as_deref());
                let metadata = probe
                    .models
                    .iter()
                    .find(|candidate| Some(candidate.id.as_str()) == selected_model);
                if model.is_some() && metadata.is_none() {
                    return Err("所选模型当前不可用。".into());
                }
                if let Some(effort) = reasoning_effort.as_ref()
                    && !metadata.is_some_and(|value| {
                        value
                            .reasoning_efforts
                            .iter()
                            .any(|option| &option.id == effort)
                    })
                {
                    return Err("该模型不支持所选思考强度。".into());
                }
                let default_effort =
                    metadata.and_then(|value| value.default_reasoning_effort.clone());
                let access = match runtime_mode.as_deref() {
                    None => None,
                    Some("ask") => Some(RuntimeMode::Ask),
                    Some("autoAcceptEdits") => Some(RuntimeMode::AutoAcceptEdits),
                    Some("fullAccess") => Some(RuntimeMode::FullAccess),
                    Some(_) => return Err("无效的权限模式。".into()),
                };
                let session = self.state.session_mut(session_id).ok_or("会话不可用。")?;
                if let Some(model) = model {
                    session.model = Some(model);
                    session.reasoning_effort = default_effort;
                    session.service_tier = None;
                    session.context_window = None;
                }
                if let Some(mode) = interaction_mode {
                    session.interaction_mode = match mode.as_str() {
                        "plan" => InteractionMode::Plan,
                        "build" => InteractionMode::Build,
                        _ => return Err("无效的会话模式。".into()),
                    };
                }
                if let Some(effort) = reasoning_effort {
                    session.reasoning_effort = Some(effort);
                }
                if let Some(access) = access {
                    session.runtime_mode = access;
                }
                session.updated_at = unix_time();
                if self.state.selected_session == Some(session_id) {
                    self.browser_access_mode_changed(cx);
                }
                self.apply_session_options(session_id, cx);
                self.save();
                cx.notify();
                Ok(json!({"accepted": true}))
            }
            fintwind_remote::RemoteCommand::CreateSession { project_id } => {
                self.remote_check_project(project_id)?;
                let session = self.state.new_session(project_id);
                let session_id = session.id;
                self.state.push_session(session);
                self.save();
                cx.notify();
                Ok(json!({"sessionId": session_id}))
            }
            fintwind_remote::RemoteCommand::UploadChunk { .. } => {
                Err("附件必须通过上传通道处理。".into())
            }
            fintwind_remote::RemoteCommand::SendPrompt {
                session_id, text, ..
            } => {
                let index = self.remote_session_index(session_id)?;
                if (text.trim().is_empty() && attachments.is_empty()) || text.len() > 32 * 1024 {
                    return Err("Prompt text is empty or too long".to_owned());
                }
                if !self.state.sessions[index].detail_loaded {
                    self.hydrate_remote_session(session_id, cx);
                    return Err("The session is loading. Refresh and try again".to_owned());
                }
                if self.submission_preparations.contains(&session_id)
                    || self.response_fork_preparations.contains_key(&session_id)
                    || self.pending_goal_actions.contains_key(&session_id)
                {
                    return Err("The session is preparing another operation".to_owned());
                }
                let prompt = text.trim();
                let end = prompt.find(char::is_whitespace).unwrap_or(prompt.len());
                if prompt[..end].eq_ignore_ascii_case("/goal") {
                    if self.state.sessions[index].provider != "opencode" { return Err("此会话不支持 Goal。".into()); }
                    let raw = prompt[end..].trim();
                    let argument = super::goal::goal_argument(raw);
                    if !attachments.is_empty() && matches!(argument.to_ascii_lowercase().as_str(), "" | "pause" | "resume" | "clear") { return Err("Goal 控制命令不能附带文件。".into()); }
                    let action = match argument.to_ascii_lowercase().as_str() {
                        "" => { self.goal_status_requests.insert(session_id); fintwind_client::goal::GoalAction::Get },
                        "pause" => fintwind_client::goal::GoalAction::Pause,
                        "resume" => fintwind_client::goal::GoalAction::Resume,
                        "clear" => fintwind_client::goal::GoalAction::Clear,
                        _ => {
                            if let Some(session) = self.state.session_mut(session_id) { session.set_title_from_prompt(argument); }
                            fintwind_client::goal::GoalAction::Start { objective: raw.to_owned(), attachments }
                        }
                    };
                    self.control_goal(session_id, action, cx);
                    self.save();
                    return Ok(json!({"accepted": true}));
                }
                self.submit_submission_for_session(
                    session_id,
                    ComposerSubmission {
                        prompt: text,
                        display_content: None,
                        attachments,
                    },
                    cx,
                );
                Ok(json!({"accepted": true}))
            }
            fintwind_remote::RemoteCommand::Stop { session_id } => {
                let index = self.remote_session_index(session_id)?;
                if self.submission_preparations.contains(&session_id) {
                    return Err("The session cannot be stopped while it is preparing".to_owned());
                }
                if !self.state.sessions[index].is_busy() {
                    return Err("The session is not running".to_owned());
                }
                self.cancel_turn_for_session(session_id, cx);
                Ok(json!({"accepted": true}))
            }
            fintwind_remote::RemoteCommand::RespondPermission {
                session_id,
                request_id,
                option_id,
            } => {
                self.remote_session_index(session_id)?;
                if self.respond_permission_for_session(session_id, request_id, option_id, cx) {
                    Ok(json!({"accepted": true}))
                } else {
                    Err("The permission request is no longer available".to_owned())
                }
            }
            fintwind_remote::RemoteCommand::AnswerQuestions {
                session_id,
                request_id,
                answers,
            } => {
                self.remote_session_index(session_id)?;
                let answers = answers
                    .into_iter()
                    .map(|answer| UserInputAnswer {
                        question_id: answer.question_id,
                        answers: answer.answers,
                    })
                    .collect();
                if self.respond_user_input_for_session(session_id, request_id, answers, true, cx) {
                    Ok(json!({"accepted": true}))
                } else {
                    Err("The questions changed or the answers are invalid".to_owned())
                }
            }
        }
    }

    fn remote_check_project(&self, project_id: Uuid) -> Result<(), String> {
        if !self.remote_project_ids.contains(&project_id)
            || !self
                .state
                .projects
                .iter()
                .any(|project| project.id == project_id)
        {
            return Err("Workspace is not available in this remote-control session".to_owned());
        }
        Ok(())
    }

    fn remote_session_index(&self, session_id: Uuid) -> Result<usize, String> {
        let index = self
            .state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
            .ok_or_else(|| "Session is not available".to_owned())?;
        let session = &self.state.sessions[index];
        self.remote_check_project(session.project_id)?;
        if !session_is_recent(session, unix_time().saturating_sub(REMOTE_WINDOW_SECS)) {
            return Err("Session is outside the 72-hour access window".to_owned());
        }
        Ok(index)
    }

    fn refresh_remote_workspace_changes(&mut self, index: usize, cx: &mut Context<Self>) {
        let session = &self.state.sessions[index];
        let session_id = session.id;
        let Some(path) = self.workspace_path_for_session(session).map(std::path::Path::to_path_buf) else { return; };
        let cache = &mut self.remote_workspace_changes;
        if cache.entries.get(&path).is_some_and(|(at, _)| at.elapsed() < REMOTE_WORKSPACE_REFRESH)
            || cache.pending.contains(&path) || cache.pending.len() >= 8 { return; }
        cache.pending.insert(path.clone());
        let generation = self.remote_generation;
        let workspace = fintwind_client::WorkspaceClient::new(self.daemon.client());
        cx.spawn(async move |this, cx| {
            let location = path.clone();
            let counts = cx.background_executor().spawn(async move {
                match workspace.request(fintwind_client::WorkspaceOperation::InspectBranches { cwd: location }) {
                    Ok(fintwind_client::WorkspaceResult::Branches { snapshot }) => snapshot.map(|snapshot| (snapshot.additions, snapshot.deletions)),
                    _ => None,
                }
            }).await;
            let _ = this.update(cx, |app, cx| {
                if app.remote_generation != generation { return; }
                app.remote_workspace_changes.pending.remove(&path);
                let Ok(index) = app.remote_session_index(session_id) else { return; };
                if app.workspace_path_for_session(&app.state.sessions[index]) != Some(path.as_path()) { return; }
                let cache = &mut app.remote_workspace_changes.entries;
                let previous = cache.get(&path).and_then(|(_, counts)| *counts);
                if cache.len() >= 8 && !cache.contains_key(&path)
                    && let Some(oldest) = cache.iter().min_by_key(|(_, (at, _))| *at).map(|(path, _)| path.clone()) {
                    cache.remove(&oldest);
                }
                cache.insert(path, (Instant::now(), counts));
                // Do not redraw the desktop or send new frames for unchanged counts.
                if previous != counts { cx.notify(); }
            });
        }).detach();
    }

    fn remote_session_snapshot(&self, index: usize) -> Value {
        self.remote_session_projection(index, false)
    }

    fn remote_session_projection(&self, index: usize, ordered_only: bool) -> Value {
        let session = &self.state.sessions[index];
        let timing = remote_message_timing(session);
        let mut remaining = 60 * 1024;
        let mut messages = Vec::new();
        let visible = session
            .messages
            .iter()
            .rev()
            .filter(|message| matches!(message.role, MessageRole::User | MessageRole::Assistant))
            .take(if ordered_only { 0 } else { REMOTE_MESSAGE_LIMIT })
            .collect::<Vec<_>>();
        let mut truncated = session.messages.len() > REMOTE_MESSAGE_LIMIT;
        for message in visible {
            let content = if message.role == MessageRole::User {
                message
                    .display_content
                    .as_deref()
                    .unwrap_or(message.content.as_str())
            } else {
                message.content.as_str()
            };
            if remaining == 0 {
                truncated = true;
                break;
            }
            let limit = REMOTE_MESSAGE_TEXT_LIMIT.min(remaining);
            truncated |= content.len() > limit;
            let text = if message.role == MessageRole::Assistant {
                bounded_tail_text(content, limit)
            } else {
                bounded_text(content, limit)
            };
            remaining = remaining.saturating_sub(text.len());
            messages.push(json!({
                "id": message.id,
                "role": match message.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    _ => continue,
                },
                "text": text,
                "createdAt": timing.get(&message.id).and_then(|(at, _)| (*at > 0).then_some(*at)),
                "elapsedSeconds": timing.get(&message.id).and_then(|(_, elapsed)| *elapsed),
            }));
        }
        messages.reverse();
        let permission_source = self
            .runtimes
            .get(&session.id)
            .and_then(|runtime| runtime.pending_permission.as_ref());
        let permission_supported = permission_source.is_none_or(|pending| {
            pending.request_id.len() <= 256
                && pending.options.len() <= 32
                && pending.options.iter().all(|option| option.id.len() <= 256)
                && pending
                    .options
                    .iter()
                    .map(|option| option.id.len() + option.label.len().min(1024) + 128)
                    .sum::<usize>()
                    + pending.request_id.len()
                    + pending.title.len().min(4096)
                    <= 8 * 1024
        });
        let permission = permission_source
            .filter(|_| permission_supported)
            .map(|pending| {
                json!({
                    "requestId": pending.request_id,
                    "title": bounded_text(&pending.title, 4096),
                    "options": pending.options.iter().take(32).map(|option| json!({
                        "id": option.id,
                        "label": bounded_text(&option.label, 1024),
                        "allow": option.allow,
                    })).collect::<Vec<_>>(),
                })
            });
        let question_source = self
            .runtimes
            .get(&session.id)
            .and_then(|runtime| runtime.pending_user_input.as_ref());
        let questions_supported = question_source.is_none_or(|pending| {
            pending.request_id.len() <= 256
                && pending.questions.len() <= 32
                && pending
                    .questions
                    .iter()
                    .map(|question| question.options.len())
                    .sum::<usize>()
                    <= 128
                && pending.questions.iter().all(|question| {
                    question.id.len() <= 256
                        && question.options.len() <= 32
                        && question
                            .options
                            .iter()
                            .all(|option| option.label.len() <= 1024)
                })
                && pending
                    .questions
                    .iter()
                    .map(|question| {
                        question.id.len()
                            + question.header.len().min(1024)
                            + question.question.len().min(4096)
                            + 256
                            + question
                                .options
                                .iter()
                                .map(|option| {
                                    option.label.len()
                                        + option
                                            .description
                                            .as_ref()
                                            .map_or(0, |text| text.len().min(1024))
                                        + 128
                                })
                                .sum::<usize>()
                    })
                    .sum::<usize>()
                    + pending.request_id.len()
                    <= 8 * 1024
        });
        let questions = question_source.filter(|_| questions_supported)
            .map(|pending| json!({
                "requestId": pending.request_id,
                "questions": pending.questions.iter().take(32).map(|question| json!({
                    "id": question.id,
                    "header": bounded_text(&question.header, 1024),
                    "question": bounded_text(&question.question, 4096),
                    "multiSelect": question.multi_select,
                    "options": question.options.iter().take(32).map(|option| json!({
                        "label": bounded_text(&option.label, 1024),
                        "description": option.description.as_deref().map(|text| bounded_text(text, 1024)),
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            }));
        let (entries, entries_truncated) = remote_transcript_entries(session, &timing);
        let mut features = vec!["orderedTranscript", "composerOptions", "attachments", "sessionDeltas", "turnMetadata", "todoList", "workspaceChanges", "sessionPermissions"];
        if session.provider == "opencode" { features.push("goals"); }
        let mut snapshot = json!({
            "id": session.id,
            "projectId": session.project_id,
            "title": bounded_text(&session.display_title(), 1024),
            "status": serde_json::to_value(session.status).unwrap_or(Value::Null),
            "messages": messages,
            "truncated": truncated || entries_truncated,
            "permission": permission,
            "questions": questions,
            "desktopRequired": !permission_supported || !questions_supported,
            "canSend": session.detail_loaded,
            "entries": entries,
            "features": features,
            "todos": remote_todo_snapshot(session),
            "workspaceChanges": self.workspace_path_for_session(session)
                .and_then(|path| self.remote_workspace_changes.entries.get(path))
                .and_then(|(_, counts)| *counts)
                .map(|(additions, deletions)| json!({"additions": additions, "deletions": deletions})),
            "goal": session.goal.as_ref().map(|goal| json!({
                "objective": bounded_text(&goal.objective, 8192), "status": goal.status,
                "turns": goal.turns, "maxTurns": goal.max_turns,
                "reason": bounded_text(&goal.reason, 2048), "result": goal.result.as_deref().map(|result| bounded_text(result, 8192)),
                "elapsedSeconds": goal.elapsed_seconds_at(unix_time()), "tools": goal.tools,
            })),
            "model": self.model_for_session(session),
            "modelName": bounded_text(self.model_metadata_for_session(session).map(|model| model.name.as_str()).or_else(|| self.model_for_session(session)).unwrap_or("OpenCode"), 256),
            "interactionMode": session.interaction_mode,
            "runtimeMode": session.runtime_mode.access(),
            "reasoningEffort": self.session_options(session).reasoning_effort,
        });
        if serde_json::to_vec(&snapshot)
            .map_or(true, |bytes| bytes.len() > REMOTE_RESPONSE_LIMIT - 1024)
        {
            snapshot["messages"] = json!([]);
            snapshot["entries"] = json!([]);
            snapshot["truncated"] = Value::Bool(true);
        }
        snapshot
    }

    fn remote_composer_snapshot(&self, index: usize, probe: &ProviderProbe) -> Value {
        let session = &self.state.sessions[index];
        let mut budget = 192 * 1024usize;
        let models = probe.models.iter().filter(|model| !model.id.is_empty() && model.id.len() <= 512)
            .take(1024).map(|model| json!({
                "id": bounded_text(&model.id, 512), "name": bounded_text(&model.name, 256),
                "provider": model.sub_provider.as_deref().map(|value| bounded_text(value, 128)),
                "efforts": model.reasoning_efforts.iter().take(32).map(|option| json!({"id": bounded_text(&option.id, 128), "label": bounded_text(&option.label, 128)})).collect::<Vec<_>>()
            })).take_while(|value| {
                let size = serde_json::to_vec(value).map_or(budget + 1, |bytes| bytes.len());
                if size > budget { return false; }
                budget -= size;
                true
            }).collect::<Vec<_>>();
        json!({
            "models": models, "canConfigure": session.detail_loaded && session.can_choose_model(),
            "commands": session.available_commands.iter().take(64).map(|command| json!({"name": bounded_text(&command.name, 128), "description": bounded_text(&command.description, 256)})).collect::<Vec<_>>()
        })
    }

    fn hydrate_remote_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        if !self.remote_session_hydrations.insert(session_id) {
            return;
        }
        let generation = self.remote_generation;
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            self.remote_session_hydrations.remove(&session_id);
            return;
        };
        let expected_updated_at = session_updated_at(session);
        let expected_cursor = session.runtime_event_cursor;
        let placement_generation = self.native_placement_generation(session_id);
        let daemon = self.daemon.clone();
        cx.spawn(async move |this, cx| {
            let hydrated =
                cx.background_executor()
                    .spawn(async move {
                        fintwind_client::persistence::hydrate_session(&daemon, session_id)
                    })
                    .await;
            let _ = this.update(cx, |app, cx| {
                if app.remote_generation != generation {
                    return;
                }
                app.remote_session_hydrations.remove(&session_id);
                if !app.remote_project_ids.contains(
                    &app.state
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .map(|session| session.project_id)
                        .unwrap_or_default(),
                ) {
                    return;
                }
                let Ok(Some(stored)) = hydrated else { return };
                let has_runtime = app.runtimes.contains_key(&session_id);
                let placement_changed = app.native_placement_generation(session_id)
                    != placement_generation
                    || app.native_placement_generation(session_id) > 0;
                let Some(current) = app.state.session_mut(session_id) else {
                    return;
                };
                if session_updated_at(current) != expected_updated_at
                    || current.detail_loaded
                    || current.runtime_event_cursor != expected_cursor
                    || (has_runtime && current.status.is_busy())
                    || (has_runtime
                        && current.runtime_event_cursor.is_some()
                        && stored.runtime_event_cursor != current.runtime_event_cursor)
                {
                    return;
                }
                if !placement_changed
                    && current.provider_cursor.is_none()
                    && current.native_session_id.is_none()
                {
                    current.workspace = stored.workspace;
                }
                current.runtime_mode = stored.runtime_mode;
                current.interaction_mode = stored.interaction_mode;
                current.reasoning_effort = stored.reasoning_effort;
                current.service_tier = stored.service_tier;
                current.context_window = stored.context_window;
                current.agent_preset = stored.agent_preset;
                current.mermaid_prompt_version = stored.mermaid_prompt_version;
                current.provider_cursor = stored.provider_cursor;
                current.available_commands = stored.available_commands;
                current.context_usage = stored.context_usage;
                current.compaction = stored.compaction;
                current.goal = stored.goal;
                current.runtime_event_cursor = stored.runtime_event_cursor;
                current.messages = stored.messages;
                current.transcript_blocks = stored.transcript_blocks;
                current.turns = stored.turns;
                current.background_work = stored.background_work;
                current.queued_messages = stored.queued_messages;
                current.detail_loaded = true;
                cx.notify();
            });
        })
        .detach();
    }
}

// Resolve the bounded message tail in one pass. A duration belongs only to the
// terminal assistant part of a settled turn, never to interim commentary.
fn remote_message_timing(session: &AgentSession) -> HashMap<Uuid, (u64, Option<u64>)> {
    let mut timing = HashMap::new();
    let mut terminal = HashMap::new();
    for message in session.messages.iter().rev().take(REMOTE_MESSAGE_LIMIT) {
        if !matches!(message.role, MessageRole::User | MessageRole::Assistant) {
            continue;
        }
        timing.insert(message.id, (message.created_at, None));
        if message.role == MessageRole::Assistant && let Some(turn_id) = message.turn_id {
            terminal.entry(turn_id).or_insert((message.id, message.streaming));
        }
    }
    for turn in &session.turns {
        let Some(&(message_id, streaming)) = terminal.get(&turn.id) else { continue; };
        if streaming || turn.status == crate::model::TurnStatus::Running { continue; }
        let Some(completed_at) = turn.completed_at.filter(|at| *at > 0) else { continue; };
        let elapsed = (turn.started_at > 0).then(|| completed_at.checked_sub(turn.started_at)).flatten();
        timing.insert(message_id, (completed_at, elapsed));
    }
    timing
}

fn remote_todo_snapshot(session: &AgentSession) -> Option<Value> {
    use super::background_work::todo::{TodoStatus, latest_snapshot};

    let snapshot = latest_snapshot(session)?;
    let mut truncated = snapshot.items.len() > 64;
    let items = snapshot.items.iter().take(64).map(|item| {
        let content = bounded_text(&item.content, 512);
        truncated |= content.len() != item.content.len();
        let status = match item.status {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Cancelled => "cancelled",
        };
        json!({"content": content, "status": status})
    }).collect::<Vec<_>>();
    Some(json!({"items": items, "completed": snapshot.completed,
        "total": snapshot.items.len(), "truncated": truncated}))
}

fn remote_transcript_entries(session: &AgentSession, timing: &HashMap<Uuid, (u64, Option<u64>)>) -> (Vec<Value>, bool) {
    enum Entry<'a> {
        Message(&'a crate::model::Message),
        Activity(&'a ActivityItem, Option<Uuid>),
    }
    let start = session.messages.len().saturating_sub(REMOTE_MESSAGE_LIMIT);
    let turns = session.turns.iter().map(|turn| (turn.id, turn)).collect::<HashMap<_, _>>();
    let mut terminal = HashMap::new();
    for message in session.messages.iter().skip(start).rev() {
        if message.role == MessageRole::Assistant && let Some(turn_id) = message.turn_id {
            terminal.entry(turn_id).or_insert(message.id);
        }
    }
    let mut ordered = Vec::new();
    for (index, message) in session.messages.iter().enumerate().skip(start) {
        if matches!(message.role, MessageRole::User | MessageRole::Assistant) {
            ordered.push((index, 1usize, (0usize, 0usize), Entry::Message(message)));
        }
    }
    for (block_index, block) in session.transcript_blocks.iter().enumerate().rev().take(200) {
        if block.after_message < start {
            continue;
        }
        let offset = block.activities.len().saturating_sub(64);
        for (index, activity) in block.activities.iter().enumerate().skip(offset) {
            ordered.push((
                block.after_message,
                0,
                (block_index, index),
                Entry::Activity(activity, block.turn_id),
            ));
        }
    }
    ordered.sort_by_key(|(anchor, kind, index, _)| (*anchor, *kind, *index));
    let mut remaining = 128 * 1024usize;
    let mut entries = Vec::new();
    let mut truncated = start > 0 || ordered.len() > 200;
    for (_, _, _, entry) in ordered.into_iter().rev().take(200) {
        let value = match entry {
            Entry::Message(message) => {
                let content = message.visible_content();
                let text = if message.role == MessageRole::Assistant {
                    bounded_tail_text(content, 16 * 1024)
                } else {
                    bounded_text(content, 16 * 1024)
                };
                truncated |= text.len() != content.len();
                let turn = message.turn_id.and_then(|id| turns.get(&id));
                let response_complete = message.role == MessageRole::Assistant && !message.streaming
                    && message.turn_id.is_none_or(|id| terminal.get(&id) == Some(&message.id)
                        && turn.is_some_and(|turn| turn.status != crate::model::TurnStatus::Running));
                json!({"type": "message", "id": message.id, "role": message.role, "text": text,
                    "turnId": message.turn_id, "turnRunning": turn.map(|turn| turn.status == crate::model::TurnStatus::Running),
                    "responseComplete": response_complete,
                    "createdAt": timing.get(&message.id).and_then(|(at, _)| (*at > 0).then_some(*at)),
                    "elapsedSeconds": timing.get(&message.id).and_then(|(_, elapsed)| *elapsed), "streaming": message.streaming,
                    "attachments": message.attachments.iter().take(8).map(|file| json!({"name": bounded_text(&file.name, 128), "isImage": file.is_image})).collect::<Vec<_>>()})
            }
            Entry::Activity(activity, turn_id) => {
                let content = if activity.kind == ActivityKind::Reasoning {
                    activity
                        .reasoning
                        .as_ref()
                        .map(|reasoning| reasoning.content.as_str())
                        .unwrap_or("")
                } else {
                    ""
                };
                let limit = 12 * 1024;
                let text = bounded_tail_text(content, limit);
                json!({"type": "activity", "id": activity.id, "kind": activity.kind,
                    "turnId": turn_id, "turnRunning": turn_id.and_then(|id| turns.get(&id)).map(|turn| turn.status == crate::model::TurnStatus::Running),
                    "title": bounded_text(&activity.title, 256),
                    "text": text, "complete": activity.complete, "failed": activity.failed,
                    "truncated": content.len() > limit})
            }
        };
        let bytes = serde_json::to_vec(&value).map_or(remaining + 1, |bytes| bytes.len());
        if bytes > remaining {
            truncated = true;
            break;
        }
        remaining -= bytes;
        entries.push(value);
    }
    entries.reverse();
    (entries, truncated)
}

fn remote_button(
    id: &'static str,
    label: String,
    variant: ButtonVariant,
    icon: IconName,
    disabled: bool,
    cx: &mut Context<Fintwind>,
    activate: impl Fn(&mut Fintwind, &mut Context<Fintwind>) + 'static,
) -> Button {
    Button::new(id, label)
        .variant(variant)
        .size(ControlSize::Lg)
        .icon(icon)
        .disabled(disabled)
        .on_click(cx.listener(move |this, _, _, cx| activate(this, cx)))
}
