//! The Devices settings page: pair a phone over the relay (invite QR,
//! check-code confirmation) and manage the grants this host has issued.
//!
//! The pairing session itself runs on a plain thread — it blocks on the
//! relay socket and on the user's decision — and reports through a
//! crossbeam channel that a detached task drains into UI state. The grant
//! file stays the single source of truth: the page re-reads it on entry
//! and after every state change, so the CLI's `--pair`/`--revoke-device`
//! and this page always agree.
//!
//! Confirmation is the security moment: the page shows the device's
//! fingerprint (the exact static key the handshake bound) and the
//! six-digit check code both ends derive from the same keys and invite.
//! Nothing is trusted without an explicit click — there is no default
//! approval anywhere on this page.
//!
//! Every mouse control is also a keyboard control: Tab reaches it, Enter
//! or Space activates it, Escape takes the safe way out, and focus is
//! visible. The QR is encoded once off the UI thread and painted as
//! quads, so the pairing card costs no per-frame element tree.

mod qr;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::app::Fintwind;
use crate::daemon::{LinkState, RemoteHostStatus};
use crate::theme::{Theme, ui_px};

/// Where the host daemon's data lives. Must agree with
/// `fintwind-remote-host`'s own default: the grant file and identity keys
/// are shared between this app and the daemon binary.
pub fn remote_data_dir() -> PathBuf {
    crate::daemon::remote_data_dir()
}

fn devices_config() -> fintwind_remote_host::HostConfig {
    let data_dir = remote_data_dir();
    fintwind_remote_host::HostConfig {
        relay_origin: std::env::var("FINTWIND_RELAY_ORIGIN")
            .unwrap_or_else(|_| "https://relay.fintwind.xyz".to_owned()),
        daemon_address: String::new(),
        daemon_token_env: String::new(),
        paired_device_file: data_dir.join("paired-device.json"),
        data_dir,
    }
}

// ── focus handles ───────────────────────────────────────────────────────
//
// The page owns its controls but not the app's entity state, so the
// per-control focus handles live in a module-local memo: one handle per
// control id, created on first render and reused for every later frame.

thread_local! {
    static CONTROL_FOCUSES: RefCell<HashMap<String, FocusHandle>> =
        RefCell::new(HashMap::new());
}

fn control_focus(key: &str, cx: &mut App) -> FocusHandle {
    CONTROL_FOCUSES.with(|handles| {
        handles
            .borrow_mut()
            .entry(key.to_owned())
            .or_insert_with(|| cx.focus_handle())
            .clone()
    })
}

/// One action any control on this page can run. Cloned into both the
/// mouse and the keyboard handler, so the two can never drift.
#[derive(Clone)]
enum DevicesAction {
    StartPairing,
    CancelPairing,
    AcceptPairing,
    DenyPairing,
    RevokeDevice(String),
}

fn run_action(
    this: &mut Fintwind,
    action: DevicesAction,
    _window: &mut Window,
    cx: &mut Context<Fintwind>,
) {
    match action {
        DevicesAction::StartPairing => start_pairing(this, cx),
        DevicesAction::CancelPairing | DevicesAction::DenyPairing => {
            answer_pairing(this, false, cx)
        }
        DevicesAction::AcceptPairing => answer_pairing(this, true, cx),
        DevicesAction::RevokeDevice(device_id) => revoke_device(this, device_id, cx),
    }
}

#[derive(Clone, Copy)]
enum ButtonTone {
    Accent,
    Neutral,
    Danger,
}

/// A plain button with the project's keyboard contract: `track_focus` +
/// `tab_index` for reachability, `focus_visible` for a visible ring,
/// Enter/Space for activation, and Escape for the safe action. A disabled
/// button keeps its shape but leaves the tab order entirely.
fn devices_button(
    id: SharedString,
    label: String,
    tone: ButtonTone,
    enabled: bool,
    action: DevicesAction,
    theme: &Theme,
    cx: &mut Context<Fintwind>,
) -> AnyElement {
    let focus = control_focus(&id, cx);
    let mut button = div()
        .id(id)
        .h(px(30.0))
        .px(px(16.0))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(ui_px(12.5))
        .cursor_default()
        .track_focus(&focus)
        .focus_visible(|style| style.border_1().border_color(theme.accent));
    button = match tone {
        ButtonTone::Accent => button.text_color(theme.on_accent).bg(theme.accent),
        ButtonTone::Neutral => button.text_color(theme.text_secondary).bg(theme.overlay),
        ButtonTone::Danger => button.text_color(theme.danger).bg(theme.overlay),
    };
    button = button.child(label);
    if enabled {
        let click_action = action.clone();
        let key_action = action.clone();
        let escape_action = action.clone();
        button = button
            .tab_index(0)
            .on_click(cx.listener(move |this, _, window, cx| {
                run_action(this, click_action.clone(), window, cx)
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "enter" | "space" => {
                        run_action(this, key_action.clone(), window, cx);
                        cx.stop_propagation();
                    }
                    // Escape never grants anything and never starts
                    // anything: on the accept button it takes the deny
                    // path, on cancel it cancels, and everywhere else it
                    // does nothing at all.
                    "escape" => {
                        match escape_action {
                            DevicesAction::AcceptPairing | DevicesAction::DenyPairing => {
                                run_action(this, DevicesAction::DenyPairing, window, cx);
                            }
                            DevicesAction::CancelPairing => {
                                run_action(this, DevicesAction::CancelPairing, window, cx);
                            }
                            DevicesAction::StartPairing | DevicesAction::RevokeDevice(_) => {}
                        }
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }));
    }
    button.into_any_element()
}

/// One in-flight pairing session and everything the UI needs to draw it.
pub(super) struct PairingSession {
    pub stage: PairingStage,
    /// Decision channel; `None` once the user answered.
    pub confirm: Option<crossbeam_channel::Sender<bool>>,
    pub events: crossbeam_channel::Receiver<fintwind_remote_host::pair::PairingEvent>,
    /// The encoded QR, built off the UI thread when the invite lands.
    /// `None` while the build runs; the page shows the scan prompt until
    /// it arrives.
    pub qr: Option<Arc<qr::QrMatrix>>,
}

/// The device a pairing session is asking about, as the handshake bound
/// it. `fingerprint` names the exact static key; `check_code` is the
/// six digits both ends derive from the same keys and invite.
#[derive(Clone, Debug)]
pub(super) struct PresentedDevice {
    pub device_name: Option<String>,
    pub fingerprint: String,
    pub check_code: String,
}

pub(super) enum PairingStage {
    /// The invite is armed; waiting for a device to scan it.
    Waiting { qr_url: String },
    /// A device completed the handshake; waiting for the user's decision.
    Confirming { device: PresentedDevice },
    /// Decision sent; waiting for the relay's link receipt.
    Finishing,
}

pub(super) fn render_devices_page(
    this: &Fintwind,
    _window: &mut Window,
    cx: &mut Context<Fintwind>,
) -> AnyElement {
    let theme = Theme::current(cx);
    let mut page = div().flex().flex_col();

    // The remote host's own state, when it is worth saying. Render reads a
    // cached snapshot only — no process or file access happens here.
    if let Some(status) = render_host_status(&theme) {
        page = page.child(status);
    }

    // ---- pairing card ----
    page = page.child(render_pairing_card(this, cx));

    // ---- error line ----
    if let Some(error) = &this.devices_error {
        page = page.child(
            div()
                .mt(px(12.0))
                .w_full()
                .px(px(20.0))
                .py(px(10.0))
                .rounded(px(13.0))
                .bg(theme.raised)
                .text_size(ui_px(12.5))
                .text_color(theme.danger)
                .child(error.clone()),
        );
    }

    // ---- grants card ----
    page = page.child(render_grants_card(this, cx));

    page.into_any_element()
}

/// The remote host's state as one line. A healthy host says nothing; a
/// failed, absent, or unreachable one is stated plainly, because a user
/// who cannot reach their phone needs to know which half is broken.
fn render_host_status(theme: &Theme) -> Option<AnyElement> {
    let (message, color) = match crate::daemon::remote_host_status() {
        RemoteHostStatus::Running { link } => match link {
            LinkState::Connected => return None,
            LinkState::Disconnected => (
                tr!("settings.devices_host_offline").to_string(),
                theme.danger,
            ),
            LinkState::Unknown => (
                tr!("settings.devices_host_unknown").to_string(),
                theme.text_secondary,
            ),
        },
        RemoteHostStatus::Starting => return None,
        RemoteHostStatus::Failed { .. } => (
            tr!("settings.devices_host_stopped").to_string(),
            theme.danger,
        ),
        RemoteHostStatus::NotStarted { .. } => (
            tr!("settings.devices_host_not_started").to_string(),
            theme.text_secondary,
        ),
    };
    Some(
        div()
            .mt(px(15.0))
            .w_full()
            .px(px(20.0))
            .text_size(ui_px(12.5))
            .text_color(color)
            .child(message)
            .into_any_element(),
    )
}

fn render_pairing_card(this: &Fintwind, cx: &mut Context<Fintwind>) -> AnyElement {
    let theme = Theme::current(cx);
    let mut card = div()
        .mt(px(15.0))
        .w_full()
        .px(px(20.0))
        .py(px(14.0))
        .rounded(px(13.0))
        .bg(theme.raised);

    card = card.child(
        div()
            .text_size(ui_px(13.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.text)
            .child(tr!("settings.devices_pair_title")),
    );

    match &this.devices_pairing {
        None => {
            card = card
                .child(
                    div()
                        .mt(px(5.0))
                        .text_size(ui_px(12.5))
                        .line_height(ui_px(18.0))
                        .text_color(theme.text_secondary)
                        .child(tr!("settings.devices_pair_description")),
                )
                .child(devices_button(
                    "devices-pair-button".into(),
                    tr!("settings.devices_pair_button").to_string(),
                    ButtonTone::Accent,
                    true,
                    DevicesAction::StartPairing,
                    &theme,
                    cx,
                ));
        }
        Some(session) => match &session.stage {
            PairingStage::Waiting { .. } => {
                card = card
                    .child(
                        div()
                            .mt(px(5.0))
                            .text_size(ui_px(12.5))
                            .text_color(theme.text_secondary)
                            .child(tr!("settings.devices_scan_prompt")),
                    )
                    .child(
                        div()
                            .mt(px(12.0))
                            .flex()
                            .items_start()
                            .gap(px(20.0))
                            .child(render_qr(session))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(ui_px(12.0))
                                    .line_height(ui_px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("settings.devices_scan_hint")),
                            ),
                    )
                    .child(devices_button(
                        "devices-cancel-button".into(),
                        tr!("settings.devices_cancel").to_string(),
                        ButtonTone::Neutral,
                        true,
                        DevicesAction::CancelPairing,
                        &theme,
                        cx,
                    ));
            }
            PairingStage::Confirming { device } => {
                let shown_name = device
                    .device_name
                    .clone()
                    .unwrap_or_else(|| tr!("settings.devices_unnamed").to_string());
                card = card
                    .child(
                        div()
                            .mt(px(5.0))
                            .text_size(ui_px(12.5))
                            .text_color(theme.text_secondary)
                            .child(tr!("settings.devices_confirm_prompt")),
                    )
                    .child(
                        div()
                            .mt(px(8.0))
                            .text_size(ui_px(12.0))
                            .line_height(ui_px(17.0))
                            .text_color(theme.text_secondary)
                            .child(tr!("settings.devices_confirm_verify")),
                    )
                    .child(
                        div()
                            .mt(px(12.0))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .text_size(ui_px(12.5))
                            .text_color(theme.text)
                            .child(labeled_line(
                                &theme,
                                tr!("settings.devices_field_name").to_string(),
                                shown_name,
                            ))
                            .child(labeled_line(
                                &theme,
                                tr!("settings.devices_fingerprint").to_string(),
                                grouped_fingerprint(&device.fingerprint),
                            )),
                    )
                    .child(
                        div()
                            .mt(px(10.0))
                            .flex()
                            .items_baseline()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .text_size(ui_px(12.5))
                                    .text_color(theme.text_secondary)
                                    .child(tr!("settings.devices_check_code")),
                            )
                            .child(
                                div()
                                    .text_size(ui_px(22.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.text)
                                    .child(device.check_code.clone()),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(10.0))
                            .text_size(ui_px(12.0))
                            .line_height(ui_px(17.0))
                            .text_color(theme.text_secondary)
                            .child(tr!("settings.devices_grant_scope")),
                    )
                    .child(
                        div()
                            .mt(px(14.0))
                            .flex()
                            .gap(px(10.0))
                            .child(devices_button(
                                "devices-confirm-yes".into(),
                                tr!("settings.devices_confirm_yes").to_string(),
                                ButtonTone::Accent,
                                true,
                                DevicesAction::AcceptPairing,
                                &theme,
                                cx,
                            ))
                            .child(devices_button(
                                "devices-confirm-no".into(),
                                tr!("settings.devices_confirm_no").to_string(),
                                ButtonTone::Neutral,
                                true,
                                DevicesAction::DenyPairing,
                                &theme,
                                cx,
                            )),
                    );
            }
            PairingStage::Finishing => {
                card = card.child(
                    div()
                        .mt(px(5.0))
                        .text_size(ui_px(12.5))
                        .text_color(theme.text_secondary)
                        .child(tr!("settings.devices_finishing")),
                );
            }
        },
    }

    card.into_any_element()
}

/// The QR: one cached element painting the pre-merged runs, or a
/// zero-size placeholder while the encode is still running.
fn render_qr(session: &PairingSession) -> AnyElement {
    match &session.qr {
        Some(matrix) => qr::QrElement::new(matrix.clone()).into_any_element(),
        None => div().into_any_element(),
    }
}

fn labeled_line(theme: &Theme, label: String, value: String) -> AnyElement {
    div()
        .flex()
        .gap(px(8.0))
        .child(
            div()
                .w(px(64.0))
                .text_color(theme.text_secondary)
                .child(label),
        )
        .child(div().min_w_0().child(value))
        .into_any_element()
}

/// Split a 32-character fingerprint into readable groups of four.
fn grouped_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_grants_card(this: &Fintwind, cx: &mut Context<Fintwind>) -> AnyElement {
    let theme = Theme::current(cx);
    let mut card = div()
        .mt(px(12.0))
        .w_full()
        .px(px(20.0))
        .py(px(14.0))
        .rounded(px(13.0))
        .bg(theme.raised)
        .child(
            div()
                .text_size(ui_px(13.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(tr!("settings.devices_list_title")),
        );

    match &this.devices_grants {
        None => {
            card = card.child(
                div()
                    .mt(px(5.0))
                    .text_size(ui_px(12.5))
                    .text_color(theme.text_secondary)
                    .child(if this.devices_busy {
                        tr!("settings.devices_loading").to_string()
                    } else {
                        tr!("settings.devices_empty").to_string()
                    }),
            );
        }
        Some((file, _)) => {
            let live: Vec<_> = file
                .grants
                .iter()
                .filter(|grant| grant.revoked_at_ms.is_none())
                .collect();
            let revoked: Vec<_> = file
                .grants
                .iter()
                .filter(|grant| grant.revoked_at_ms.is_some())
                .collect();
            if live.is_empty() && revoked.is_empty() {
                card = card.child(
                    div()
                        .mt(px(5.0))
                        .text_size(ui_px(12.5))
                        .text_color(theme.text_secondary)
                        .child(tr!("settings.devices_empty")),
                );
            }
            for grant in &live {
                card = card.child(grant_row(
                    this,
                    grant.device_id.clone(),
                    grant.display_label(),
                    grouped_fingerprint(&grant.fingerprint()),
                    false,
                    cx,
                ));
            }
            for grant in &revoked {
                card = card.child(grant_row(
                    this,
                    grant.device_id.clone(),
                    grant.display_label(),
                    grouped_fingerprint(&grant.fingerprint()),
                    true,
                    cx,
                ));
            }
        }
    }

    card.into_any_element()
}

/// One device row: the confirmed name (or its fingerprint when the device
/// never gave one), the fingerprint detail, and the revoke control. A
/// revoked row keeps the identity visible instead of pretending the
/// device never existed.
fn grant_row(
    this: &Fintwind,
    device_id: String,
    label: String,
    fingerprint: String,
    revoked: bool,
    cx: &mut Context<Fintwind>,
) -> AnyElement {
    let theme = Theme::current(cx);
    let revoking = this.devices_revoking.as_deref() == Some(device_id.as_str());
    let row = div()
        .mt(px(10.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(ui_px(12.5))
                        .text_color(if revoked {
                            theme.text_ghost
                        } else {
                            theme.text
                        })
                        .child(label),
                )
                .child(if revoked {
                    div()
                        .text_size(ui_px(12.0))
                        .text_color(theme.text_ghost)
                        .child(tr!("settings.devices_revoked_tag").to_string())
                        .into_any_element()
                } else if revoking {
                    div()
                        .text_size(ui_px(12.0))
                        .text_color(theme.text_secondary)
                        .child(tr!("settings.devices_revoking").to_string())
                        .into_any_element()
                } else {
                    devices_button(
                        SharedString::from(format!("devices-revoke-{device_id}")),
                        tr!("settings.devices_revoke").to_string(),
                        ButtonTone::Danger,
                        !this.devices_busy,
                        DevicesAction::RevokeDevice(device_id.clone()),
                        &theme,
                        cx,
                    )
                }),
        )
        .child(
            div()
                .text_size(ui_px(11.0))
                .text_color(theme.text_ghost)
                .child(fingerprint),
        );
    row.into_any_element()
}

// ── actions ─────────────────────────────────────────────────────────────

fn start_pairing(this: &mut Fintwind, cx: &mut Context<Fintwind>) {
    if this.devices_pairing.is_some() {
        return;
    }
    let config = Arc::new(devices_config());
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let (confirm_tx, confirm_rx) = crossbeam_channel::bounded(1);
    let worker_config = config.clone();
    std::thread::Builder::new()
        .name("pairing-session".to_owned())
        .spawn(move || {
            let result = fintwind_remote_host::pair::run_pairing_session_events(
                &worker_config,
                &event_tx,
                &confirm_rx,
            );
            if let Err(error) = result {
                // The terminal event: the pump turns this into UI state.
                let _ = event_tx.send(fintwind_remote_host::pair::PairingEvent::Failed(
                    error.to_string(),
                ));
            }
        })
        .expect("pairing thread spawns");

    this.devices_pairing = Some(PairingSession {
        stage: PairingStage::Waiting {
            qr_url: String::new(),
        },
        confirm: Some(confirm_tx),
        events: event_rx,
        qr: None,
    });
    this.devices_error = None;
    cx.notify();
    pump_pairing_events(cx);
}

fn answer_pairing(this: &mut Fintwind, accepted: bool, cx: &mut Context<Fintwind>) {
    if let Some(session) = &mut this.devices_pairing {
        if let Some(confirm) = session.confirm.take() {
            let _ = confirm.send(accepted);
        }
        if accepted {
            session.stage = PairingStage::Finishing;
        } else {
            // Rejected or cancelled: the session is over either way. The
            // thread unwinds itself (the wait loop polls the confirm
            // channel; the gate consumed the decision) — the view returns
            // to the initial state immediately instead of waiting for it.
            this.devices_pairing = None;
            this.refresh_device_grants(cx);
        }
    }
    cx.notify();
}

/// Revoke one device. The local grant goes first and is the reliable
/// boundary; a relay unlink that does not confirm is reported as pending
/// — the row shows revoked and the page says the relay side is open.
fn revoke_device(this: &mut Fintwind, device_id: String, cx: &mut Context<Fintwind>) {
    if this.devices_busy {
        return;
    }
    this.devices_busy = true;
    this.devices_revoking = Some(device_id.clone());
    cx.notify();
    let config = devices_config();
    cx.spawn(async move |this, cx| {
        let id = device_id.clone();
        let result = cx
            .background_executor()
            .spawn(
                async move { fintwind_remote_host::pair::revoke_device_with_outcome(&config, &id) },
            )
            .await;
        this.update(cx, |this, cx| {
            this.devices_busy = false;
            this.devices_revoking = None;
            this.devices_grants = None;
            match result {
                Ok(fintwind_remote_host::pair::RevokeOutcome::Revoked) => {
                    this.devices_error = None;
                }
                Ok(fintwind_remote_host::pair::RevokeOutcome::RelayPending { reason }) => {
                    this.devices_error = Some(format!(
                        "{}（{reason}）",
                        tr!("settings.devices_revoke_pending")
                    ));
                }
                Err(error) => {
                    this.devices_error = Some(error.to_string());
                }
            }
            this.refresh_device_grants(cx);
            cx.notify();
        })
        .ok();
    })
    .detach();
}

/// What one pump pass produced, so the QR encode can be started outside
/// the entity update.
enum PumpStep {
    Idle,
    Invite { qr_url: String },
    Finished,
}

fn pump_pairing_events(cx: &mut Context<Fintwind>) {
    // The QR encode runs on a background executor and lands here; the
    // loop parks it on the session when it is ready.
    let (qr_tx, qr_rx) = crossbeam_channel::unbounded::<Option<Arc<qr::QrMatrix>>>();
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let step = this
                .update(cx, |this, cx| {
                    // Clone the receiver out first: the drain loop below
                    // must be free to reset `devices_pairing` while it
                    // handles a terminal event.
                    let Some(events) = this.devices_pairing.as_ref().map(|s| s.events.clone())
                    else {
                        return PumpStep::Finished;
                    };
                    let mut step = PumpStep::Idle;
                    while let Ok(event) = events.try_recv() {
                        match event {
                            fintwind_remote_host::pair::PairingEvent::InviteReady(invite) => {
                                let qr_url = fintwind_remote_host::pair::pairing_url(&invite);
                                if let Some(session) = &mut this.devices_pairing {
                                    if let PairingStage::Waiting { qr_url: slot } =
                                        &mut session.stage
                                    {
                                        *slot = qr_url.clone();
                                    }
                                }
                                step = PumpStep::Invite { qr_url };
                            }
                            fintwind_remote_host::pair::PairingEvent::DevicePresented {
                                device_id: _,
                                device_name,
                                fingerprint,
                                check_code,
                            } => {
                                if let Some(session) = &mut this.devices_pairing {
                                    session.stage = PairingStage::Confirming {
                                        device: PresentedDevice {
                                            device_name,
                                            fingerprint,
                                            check_code,
                                        },
                                    };
                                }
                            }
                            fintwind_remote_host::pair::PairingEvent::Linked { device_id: _ } => {
                                this.devices_pairing = None;
                                this.devices_grants = None;
                                this.refresh_device_grants(cx);
                                this.devices_error = None;
                                return PumpStep::Finished;
                            }
                            fintwind_remote_host::pair::PairingEvent::Failed(reason) => {
                                this.devices_pairing = None;
                                this.devices_error = Some(reason);
                                return PumpStep::Finished;
                            }
                        }
                    }
                    cx.notify();
                    if this.devices_pairing.is_none() {
                        return PumpStep::Finished;
                    }
                    step
                })
                .ok();
            match step {
                Some(PumpStep::Finished) | None => break,
                Some(PumpStep::Idle) => {}
                Some(PumpStep::Invite { qr_url }) => {
                    let qr_tx = qr_tx.clone();
                    cx.background_executor()
                        .spawn(async move {
                            let matrix = qr::QrMatrix::new(&qr_url).ok().map(Arc::new);
                            let _ = qr_tx.send(matrix);
                        })
                        .detach();
                }
            }
            // The encode is milliseconds of work; park the result on the
            // session as soon as it lands. A failed encode is stated, not
            // swallowed: without the QR the invite cannot be scanned.
            if let Ok(built) = qr_rx.try_recv() {
                let landed = this
                    .update(cx, |this, cx| {
                        match built {
                            Some(matrix) => {
                                if let Some(session) = &mut this.devices_pairing {
                                    session.qr = Some(matrix);
                                }
                            }
                            None => {
                                this.devices_error =
                                    Some(tr!("settings.devices_qr_failed").to_string());
                            }
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !landed {
                    break;
                }
            }
        }
    })
    .detach();
}

impl Fintwind {
    /// Re-read the grant file off-thread and publish it for the devices
    /// page. `None` keeps the page on its empty/loading line until the
    /// read lands.
    pub(super) fn refresh_device_grants(&mut self, cx: &mut Context<Self>) {
        let config = devices_config();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    fintwind_remote_host::GrantFile::load(&config.paired_device_file)
                        .ok()
                        .flatten()
                })
                .await;
            this.update(cx, |this, cx| {
                this.devices_grants = loaded.map(|file| (file, Instant::now()));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
