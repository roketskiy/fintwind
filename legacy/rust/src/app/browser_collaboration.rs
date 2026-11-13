//! Desktop half of the browser bridge.
//!
//! Pages are addressed by page id only. There is no session, runtime or
//! grant binding, no launcher capability per session, and no approval:
//! tools have full access to every published page. Requests arrive on a
//! direct, per-connection inbox — never through the replayable driver
//! event stream — and are answered by request id.
//!
//! Threading: one dedicated bridge thread owns the blocking receives, so the
//! UI thread never waits on a socket. It forwards work into a bounded wake
//! channel that a `cx.spawn` loop drains — deliberately not the stream event
//! pump, so browser traffic can never delay a streaming frame. Published
//! share sets and request completions are handed straight to the daemon
//! client, which itself only validates fields and performs a lock-free
//! enqueue onto its own writer thread. The GUI is the single sender, so
//! call order is wire order and nothing can reorder a grant against its
//! revocation; none of this is reachable from a frame, so no render pays for
//! it.

use std::collections::HashMap;

use crossbeam_channel::Receiver;
use gpui::{Context, Entity, Subscription, Task};
use uuid::Uuid;

use super::*;
use crate::browser::{BrowserCollaborationEvent, BrowserView};
use fintwind_client::{BrowserNotification, DaemonClient};
use fintwind_protocol::browser::{
    BrowserAction, BrowserRequest, BrowserResult, BrowserScope, BrowserShare,
    MAX_BROWSER_PAGES_PER_CONNECTION,
};

mod automation;

/// Wake-queue bound. The bridge thread blocks here when the UI is behind,
/// which is the only backpressure this bridge applies.
const WAKE_QUEUE_BOUND: usize = 128;

/// Work handed from the bridge thread to the UI thread.
enum BridgeWake {
    /// A daemon client became active (the first one, or a replacement).
    Client {
        client: DaemonClient,
        generation: u64,
    },
    /// One live-only browser notification, tagged with the connection that
    /// delivered it.
    Notification {
        generation: u64,
        notification: BrowserNotification,
    },
}

/// All browser state the app owns. Page shares live on the
/// `BrowserView` entities; this holds the bridge, the subscriptions and the
/// client each in-flight request must be answered on.
pub(super) struct AppBrowserCollaborationState {
    /// The set last successfully published. Only written after the client
    /// accepted the publish, so a failed attempt cannot leave the app
    /// believing in pages the daemon never took.
    shares: HashMap<Uuid, BrowserShare>,
    /// One subscription per live `BrowserView`, keyed by page id, so a closed
    /// tab stops delivering events.
    subscriptions: HashMap<Uuid, Subscription>,
    /// In-flight requests by id, with the connection that must answer them.
    /// A reconnected connection never accepts an answer minted for its
    /// predecessor, which the client itself enforces.
    pending: HashMap<Uuid, DaemonClient>,
    /// The wake-loop task for the bridge thread. It lives as long as the app
    /// entity; the bridge thread exits once this channel's receiver drops.
    task: Task<()>,
    /// Bumped for every daemon client, so a late receipt from a previous
    /// connection is refused rather than applied to the new pages.
    generation: u64,
    /// The client the current bridge publishes to. `None` between a
    /// disconnect and the supervisor's replacement, and after a failed
    /// publish until then.
    client: Option<DaemonClient>,
    /// Global launcher for tool opens. It is
    /// not a browser page and never appears in the shared-page list.
    host: Option<BrowserScope>,
    automation_pages: std::collections::HashSet<Uuid>,
    /// A cancelled opening must never share its page when loading finishes.
    openings: HashMap<Uuid, Uuid>,
    /// Keep completed opens linked to their exact page grant while it lives:
    /// a cancellation queued behind the ready timer can still withdraw a
    /// page published just before the daemon rejected the late completion.
    opened_pages: HashMap<Uuid, BrowserScope>,
    /// Dropping this stops the bridge thread even while it blocks.
    stop_tx: crossbeam_channel::Sender<()>,
}

impl AppBrowserCollaborationState {
    pub(super) fn new() -> Self {
        let (stop_tx, _) = crossbeam_channel::bounded::<()>(1);
        Self {
            shares: HashMap::new(),
            subscriptions: HashMap::new(),
            pending: HashMap::new(),
            task: Task::ready(()),
            generation: 0,
            client: None,
            host: None,
            automation_pages: Default::default(),
            openings: HashMap::new(),
            opened_pages: HashMap::new(),
            stop_tx,
        }
    }
}

impl Drop for AppBrowserCollaborationState {
    fn drop(&mut self) {
        // `try_send`, never `send`: a queued stop is already enough, and the
        // bridge thread must not be able to block the app's teardown.
        let _ = self.stop_tx.try_send(());
    }
}

/// Blocking bridge: one daemon connection at a time. Follows the same shape
/// as the task-state sync worker — a dedicated thread owns the crossbeam
/// receives, and every outcome crosses to the UI through the bounded wake
/// channel, where `send_blocking` is safe because this thread is not the UI.
fn run_browser_bridge(
    clients: Receiver<DaemonClient>,
    wake: smol::channel::Sender<BridgeWake>,
    stop: crossbeam_channel::Receiver<()>,
) {
    let mut client = crossbeam_channel::select! {
        recv(stop) -> _ => return,
        recv(clients) -> client => match client {
            Ok(client) => client,
            Err(_) => return,
        },
    };
    let mut generation: u64 = 0;
    loop {
        generation = generation.saturating_add(1);
        if wake
            .send_blocking(BridgeWake::Client {
                client: client.clone(),
                generation,
            })
            .is_err()
        {
            return;
        }
        let requests = client.subscribe_browser_requests();
        // Either the supervisor publishes a replacement client, this
        // connection's browser inbox dies with the socket, or the app ends.
        client = loop {
            crossbeam_channel::select! {
                recv(stop) -> _ => return,
                recv(clients) -> replacement => {
                    let Ok(mut replacement) = replacement else {
                        return;
                    };
                    while let Ok(newer) = clients.try_recv() {
                        replacement = newer;
                    }
                    break replacement;
                }
                recv(requests) -> notification => match notification {
                    Ok(notification) => {
                        if wake
                            .send_blocking(BridgeWake::Notification {
                                generation,
                                notification,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => {
                        // The inbox closed with the socket, so anything the
                        // daemon had granted on this connection is gone.
                        // Revoke locally first — waiting for the replacement
                        // client before saying so would leave the UI showing
                        // a share that no longer exists anywhere.
                        if wake
                            .send_blocking(BridgeWake::Notification {
                                generation,
                                notification: BrowserNotification::Disconnected,
                            })
                            .is_err()
                        {
                            return;
                        }
                        let replacement = crossbeam_channel::select! {
                            recv(stop) -> _ => return,
                            recv(clients) -> replacement => match replacement {
                                Ok(replacement) => replacement,
                                Err(_) => return,
                            },
                        };
                        let mut replacement = replacement;
                        while let Ok(newer) = clients.try_recv() {
                            replacement = newer;
                        }
                        break replacement;
                    }
                },
            }
        };
    }
}

impl Fintwind {
    /// Start the bridge once the app entity exists. Called once from
    /// [`Fintwind::new`]. A live local session publishes a launcher and its
    /// own available browser tabs, independently of the session mode.
    pub(super) fn start_browser_collaboration(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let clients = self.daemon.subscribe_clients();
        let (wake_tx, wake_rx) = smol::channel::bounded::<BridgeWake>(WAKE_QUEUE_BOUND);
        let stop_rx = {
            let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
            self.browser_collaboration.stop_tx = stop_tx;
            stop_rx
        };
        std::thread::Builder::new()
            .name("fintwind-browser-collaboration".into())
            .spawn(move || run_browser_bridge(clients, wake_tx, stop_rx))
            .ok();

        // A wake loop, not the stream event pump: browser traffic must never
        // delay or be delayed by a streaming frame.
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(wake) = wake_rx.recv().await {
                let alive = this
                    .update_in(cx, |this, window, cx| {
                        this.handle_browser_bridge_wake(wake, window, cx)
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        });
        self.browser_collaboration.task = task;
    }

    fn handle_browser_bridge_wake(
        &mut self,
        wake: BridgeWake,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match wake {
            BridgeWake::Client { client, generation } => {
                self.rebind_browser_collaboration(client, generation, cx);
            }
            BridgeWake::Notification {
                generation,
                notification,
            } => {
                // A receipt from a connection that has since been replaced
                // describes a world that no longer exists.
                if generation != self.browser_collaboration.generation {
                    return true;
                }
                match notification {
                    BrowserNotification::Request(request) => {
                        self.dispatch_browser_request(request, generation, window, cx);
                    }
                    BrowserNotification::Cancel(request_id) => {
                        self.cancel_browser_request(request_id, cx);
                    }
                    // The connection died. Published pages go with it; the
                    // bridge thread waits for the supervisor's replacement,
                    // so this loop stays alive rather than treating a
                    // reconnect as shutdown.
                    BrowserNotification::Disconnected => {
                        self.drop_browser_connection(cx);
                    }
                    // The daemon refused one or more of this connection's
                    // published pages or the launcher registration
                    // (validation only). Revoke the affected pages by page
                    // id so a rejection that crossed a re-share cannot take
                    // a newer share.
                    BrowserNotification::ShareRejected { scopes, message } => {
                        let host_rejected =
                            self.browser_collaboration
                                .host
                                .as_ref()
                                .is_some_and(|host| {
                                    scopes.iter().any(|scope| scope.page_id == host.page_id)
                                });
                        if host_rejected {
                            self.pause_browser_automation(cx);
                            self.withdraw_browser_automation_host();
                        }
                        let revoked = self.revoke_receipt_scopes(&scopes, cx);
                        if revoked || host_rejected {
                            self.show_toast(tr!(
                                "browser_collaboration.share_rejected",
                                message = message
                            ));
                        }
                    }
                }
            }
        }
        true
    }

    /// Adopt a daemon client. A reconnect never inherits pages: every
    /// share is revoked first and the previous connection's consumer is
    /// already retired by the bridge thread.
    fn rebind_browser_collaboration(
        &mut self,
        client: DaemonClient,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        self.drop_browser_connection(cx);
        self.browser_collaboration.generation = generation;
        self.browser_collaboration.client = Some(client);
        self.sync_browser_automation_host(cx);
    }

    /// Drop the current connection's publishing ability and every page.
    /// Used on disconnect, on a failed publish, and before a rebind.
    fn drop_browser_connection(&mut self, cx: &mut Context<Self>) {
        self.browser_collaboration.client = None;
        self.browser_collaboration.host = None;
        self.browser_collaboration.shares.clear();
        self.revoke_all_browser_shares(cx);
        self.browser_collaboration.pending.clear();
    }

    /// Subscribe to one browser entity's collaboration events. Called where
    /// the entity is created, with the page id as the key.
    pub(super) fn observe_browser_collaboration(
        &mut self,
        browser_id: Uuid,
        browser: &Entity<BrowserView>,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe(
            browser,
            move |this: &mut Self, _, event: &BrowserCollaborationEvent, cx| {
                this.handle_browser_collaboration_event(browser_id, event, cx);
            },
        );
        self.browser_collaboration
            .subscriptions
            .insert(browser_id, subscription);
    }

    /// Drop one browser's subscription and grant bookkeeping. The entity
    /// itself is gone, so no `ShareChanged` will arrive for it.
    pub(super) fn forget_browser_collaboration(
        &mut self,
        browser_id: Uuid,
        cx: &mut Context<Self>,
    ) {
        self.retire_browser_collaboration(browser_id);
        self.publish_browser_shares(cx);
        self.sync_browser_automatic_pages(cx);
    }

    /// Drop one browser's subscription and publish the last set without it.
    /// Used where no `Context` is at hand (session removal); the cache is
    /// the last published snapshot, so this stays correct without entities.
    pub(super) fn retire_browser_collaboration(&mut self, browser_id: Uuid) {
        self.browser_collaboration
            .automation_pages
            .remove(&browser_id);
        if self
            .browser_collaboration
            .openings
            .values()
            .any(|id| *id == browser_id)
        {
            let retired = self
                .browser_collaboration
                .openings
                .iter()
                .filter_map(|(id, page)| (*page == browser_id).then_some(*id))
                .collect::<Vec<_>>();
            for id in retired {
                self.finish_browser_opening(
                    id,
                    BrowserResult::error("The browser tab was closed during opening."),
                );
            }
        }
        self.browser_collaboration.subscriptions.remove(&browser_id);
        let pages = self
            .browser_collaboration
            .shares
            .values()
            .filter(|share| share.scope.page_id != browser_id)
            .cloned()
            .collect::<Vec<_>>();
        if pages.len() == self.browser_collaboration.shares.len() {
            // The page was never published, so the daemon has nothing to
            // unlearn about it.
            return;
        }
        if let Some(error) = self.publish_share_set(pages) {
            // No `Context` here to revoke the views. The dead connection's
            // `Disconnected` receipt — sent by the client thread as the
            // socket closes — performs exactly that revocation, so the grants
            // cannot outlive it.
            self.browser_collaboration.client = None;
            self.browser_collaboration.shares.clear();
            self.show_toast(tr!("browser_collaboration.publish_failed", error = error));
        }
    }

    /// Prune subscriptions for pages that no longer exist in any session.
    /// The share cache is deliberately left alone: the next publish rebuilds
    /// it from the entities that survive and must notice the missing page.
    pub(super) fn retain_browser_collaboration(
        &mut self,
        retained: &std::collections::HashSet<Uuid>,
    ) {
        self.browser_collaboration
            .subscriptions
            .retain(|browser_id, _| retained.contains(browser_id));
    }

    fn handle_browser_collaboration_event(
        &mut self,
        browser_id: Uuid,
        event: &BrowserCollaborationEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            BrowserCollaborationEvent::ShareRequested => {}
            BrowserCollaborationEvent::PageReady => self.sync_browser_automatic_pages(cx),
            BrowserCollaborationEvent::ShareChanged => {
                if self
                    .right_panel_browsers
                    .get(&browser_id)
                    .is_none_or(|browser| browser.read(cx).browser_share().is_none())
                {
                    self.browser_collaboration
                        .automation_pages
                        .remove(&browser_id);
                }
                self.publish_browser_shares(cx);
            }
            BrowserCollaborationEvent::Finished { request_id, result } => {
                // Answer on the connection that delivered the request; a
                // reconnected connection refuses the late completion anyway.
                let Some(client) = self.browser_collaboration.pending.remove(request_id) else {
                    return;
                };
                let _ = client.complete_browser_request(*request_id, result.clone());
            }
        }
    }

    /// Publish the live share set. The entities are the source of truth, so
    /// this re-reads them rather than trusting a cached map; the cache is
    /// only written once the client accepted the publish.
    fn publish_browser_shares(&mut self, cx: &mut Context<Self>) {
        let mut shares = Vec::new();
        for browser in self.right_panel_browsers.values() {
            if let Some(share) = browser.read(cx).browser_share() {
                shares.push(share);
            }
        }
        self.browser_collaboration
            .opened_pages
            .retain(|_, scope| shares.iter().any(|share| share.scope == *scope));
        if let Some(error) = self.publish_share_set(shares) {
            self.fail_browser_connection(error, cx);
        }
    }

    /// Hand one share set to the daemon client. `Some(error)` means the
    /// connection is unusable and the caller must tear it down; the cache is
    /// never written on that path, so the app cannot believe in a grant the
    /// daemon refused.
    fn publish_share_set(&mut self, shares: Vec<BrowserShare>) -> Option<String> {
        let unchanged = shares.len() == self.browser_collaboration.shares.len()
            && shares.iter().all(|share| {
                self.browser_collaboration
                    .shares
                    .get(&share.scope.page_id)
                    .is_some_and(|previous| {
                        previous.scope.page_id == share.scope.page_id
                            && previous.url == share.url
                            && previous.title == share.title
                    })
            });
        if unchanged {
            return None;
        }
        let client = self.browser_collaboration.client.clone()?;
        match client.publish_browser_pages(shares.clone()) {
            Ok(()) => {
                self.browser_collaboration.shares = shares
                    .iter()
                    .map(|share| (share.scope.page_id, share.clone()))
                    .collect();
                None
            }
            Err(error) => Some(error.to_string()),
        }
    }

    /// A publish the client would not even enqueue: the connection is dead.
    /// Clearing the client first is what stops the recursion the revocations
    /// below would otherwise start — their `ShareChanged` events re-enter the
    /// publish path and find no client to publish on.
    fn fail_browser_connection(&mut self, error: String, cx: &mut Context<Self>) {
        self.browser_collaboration.host = None;
        self.browser_collaboration.client = None;
        self.browser_collaboration.shares.clear();
        self.revoke_all_browser_shares(cx);
        self.show_toast(tr!("browser_collaboration.publish_failed", error = error));
        cx.notify();
    }

    /// Apply the daemon's validation rejection. A page is revoked when its
    /// page id is listed, so a stale receipt cannot take a newer share.
    fn revoke_receipt_scopes(&mut self, scopes: &[BrowserScope], cx: &mut Context<Self>) -> bool {
        let mut revoked_any = false;
        for scope in scopes {
            let Some(browser) = self.right_panel_browsers.get(&scope.page_id).cloned() else {
                continue;
            };
            let matches = browser
                .read(cx)
                .browser_share()
                .is_some_and(|share| share.scope.page_id == scope.page_id);
            if matches {
                browser.update(cx, |view, cx| view.revoke_browser_share(cx));
                revoked_any = true;
            }
        }
        revoked_any
    }

    /// Route one daemon-delivered request to the page it names. Pages match
    /// by page id only: there is no session, runtime or grant check.
    fn dispatch_browser_request(
        &mut self,
        request: BrowserRequest,
        generation: u64,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if generation != self.browser_collaboration.generation {
            return;
        }
        let Some(client) = self.browser_collaboration.client.clone() else {
            // No live connection to answer on; the daemon-side caller times
            // out on its own, which is the honest outcome here.
            return;
        };
        if client.is_disconnected() {
            return;
        }
        // A WebView action may need to select its tab. Never let a concurrent
        // request switch to another tab while the first action is in flight.
        if !self.browser_collaboration.pending.is_empty() {
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error(
                    "Another browser operation is in progress; no action was issued.",
                ),
            );
            return;
        }
        if let BrowserAction::Open { url } = &request.action {
            self.open_automation_browser(request.clone(), url.clone(), generation, window, cx);
            return;
        }
        if matches!(request.action, BrowserAction::Close) {
            // Launcher-side like `Open`: a page-level grant must never close
            // its own surface, so the app host answers this synchronously and
            // nothing is parked in the pending map.
            self.close_automation_browser(request, cx);
            return;
        }
        let scope = request.scope.clone();
        let Some(browser) = self.right_panel_browsers.get(&scope.page_id).cloned() else {
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error("The browser page is no longer open."),
            );
            return;
        };
        let published = browser
            .read(cx)
            .browser_share()
            .is_some_and(|share| share.scope.page_id == scope.page_id);
        if !published {
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error("This browser page is no longer published."),
            );
            return;
        }
        self.browser_collaboration
            .pending
            .insert(request.request_id, client);
        browser.update(cx, |view, cx| view.ensure_automation_document(cx));
        if browser.read(cx).browser_automation_visible() {
            browser.update(cx, |view, cx| view.handle_browser_request(request, cx));
            return;
        }
        // Native WebView input requires the exact tab to be visible. Select it
        // and wait for the next render rather than attempting a hidden surface.
        self.open_right_panel_surface(RightPanelSurface::Browser(scope.page_id), cx);
        self.right_panel_pending_browser_focus = None;
        cx.spawn_in(window, async move |this, cx| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(30))
                    .await;
                let done = this
                    .update_in(cx, |this, _, cx| {
                        if !this
                            .browser_collaboration
                            .pending
                            .contains_key(&request.request_id)
                        {
                            return true;
                        }
                        let current = this.browser_collaboration.generation == generation
                            && this
                                .right_panel_browsers
                                .get(&scope.page_id)
                                .is_some_and(|page| {
                                    page.read(cx)
                                        .browser_share()
                                        .is_some_and(|share| share.scope.page_id == scope.page_id)
                                });
                        if !current || std::time::Instant::now() >= deadline {
                            if let Some(client) = this
                                .browser_collaboration
                                .pending
                                .remove(&request.request_id)
                            {
                                let message = if current {
                                    "The browser page did not become visible."
                                } else {
                                    "The browser page changed before execution."
                                };
                                let _ = client.complete_browser_request(
                                    request.request_id,
                                    BrowserResult::error(message),
                                );
                            }
                            return true;
                        }
                        if browser.read(cx).browser_automation_visible() {
                            browser.update(cx, |view, cx| {
                                view.handle_browser_request(request.clone(), cx)
                            });
                            return true;
                        }
                        false
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    /// The daemon gave up on a request: retire it wherever it sits.
    fn cancel_browser_request(&mut self, request_id: Uuid, cx: &mut Context<Self>) {
        self.browser_collaboration.pending.remove(&request_id);
        if let Some(page_id) = self.browser_collaboration.openings.remove(&request_id)
            && let Some(browser) = self.right_panel_browsers.get(&page_id)
        {
            browser.update(cx, |view, cx| view.cancel_browser_opening(cx));
        }
        if let Some(scope) = self.browser_collaboration.opened_pages.remove(&request_id) {
            let still_shared =
                self.right_panel_browsers
                    .get(&scope.page_id)
                    .is_some_and(|browser| {
                        browser
                            .read(cx)
                            .browser_share()
                            .is_some_and(|share| share.scope.page_id == scope.page_id)
                    });
            if still_shared {
                if let Some(browser) = self.right_panel_browsers.get(&scope.page_id) {
                    browser.update(cx, |view, cx| view.cancel_browser_opening(cx));
                }
                self.browser_collaboration
                    .automation_pages
                    .remove(&scope.page_id);
                self.revoke_browser_grant(scope.page_id, cx);
            }
        }
        for browser in self.right_panel_browsers.values() {
            browser.update(cx, |view, cx| view.cancel_browser_request(request_id, cx));
        }
    }

    /// Revoke every live page. Used on tab removal and daemon reconnects.
    /// Revoking cancels in-flight operations; their already-issued side
    /// effects may still have happened. The share cache is touched by the
    /// publication those revocations trigger.
    pub(super) fn revoke_all_browser_shares(&mut self, cx: &mut Context<Self>) {
        self.withdraw_browser_automation_host();
        self.pause_browser_automation(cx);
        self.browser_collaboration.opened_pages.clear();
        self.browser_collaboration.automation_pages.clear();
        let browsers = self
            .right_panel_browsers
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for browser in browsers {
            browser.update(cx, |view, cx| view.revoke_browser_share(cx));
        }
        self.browser_collaboration.pending.clear();
    }

    /// Revoke one page's share, by page id.
    fn revoke_browser_grant(&mut self, page_id: Uuid, cx: &mut Context<Self>) {
        if let Some(browser) = self.right_panel_browsers.get(&page_id).cloned() {
            // The view's own `ShareChanged` publishes the reduced set — the
            // cache is still holding this page at that moment, so the daemon
            // is told about the removal.
            browser.update(cx, |view, cx| view.revoke_browser_share(cx));
            return;
        }
        // No entity left to announce the change: publish the cache as it
        // stands minus this page.
        let pages = self
            .browser_collaboration
            .shares
            .values()
            .filter(|share| share.scope.page_id != page_id)
            .cloned()
            .collect::<Vec<_>>();
        if pages.len() == self.browser_collaboration.shares.len() {
            return;
        }
        if let Some(error) = self.publish_share_set(pages) {
            self.fail_browser_connection(error, cx);
        }
    }
}
