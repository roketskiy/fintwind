//! Desktop half of the browser collaboration bridge.
//!
//! A grant names one session, one runtime, one page and one random grant id;
//! nothing here addresses a "current page" globally, and a grant never
//! outlives the session, runtime, page or daemon connection it was issued
//! for. Requests arrive on a direct, per-connection inbox — never through
//! the replayable driver event stream — and are answered by request id.
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
    BrowserRequest, BrowserResult, BrowserScope, BrowserShare, MAX_BROWSER_PAGES_PER_CONNECTION,
};

/// Wake-queue bound. The bridge thread blocks here when the UI is behind,
/// which is the only backpressure this bridge applies.
const WAKE_QUEUE_BOUND: usize = 128;

/// Why the shared-page control is unavailable right now. The header shows
/// the reason instead of disabling silently.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BrowserShareBlocker {
    /// No task is selected, so there is no session to bind a grant to.
    NoSession,
    /// The selected task has no live runtime to bind a grant to.
    NoRuntime,
    /// No live daemon connection to publish a grant on.
    NoConnection,
    /// The connection already publishes as many pages as the daemon takes.
    TooManyPages,
}

/// What the right-panel header's browser control does for the active page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BrowserShareAffordance {
    /// Share the page with the selected session's live runtime.
    Share,
    /// Take the page back: revoke the grant and cancel pending work.
    Revoke,
    /// Sharing is impossible; the blocker says which precondition is missing.
    Blocked(BrowserShareBlocker),
}

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

/// All browser-collaboration state the app owns. Grants live on the
/// `BrowserView` entities; this holds the bridge, the subscriptions and the
/// client each in-flight request must be answered on.
pub(super) struct AppBrowserCollaborationState {
    /// The set last successfully published. Only written after the client
    /// accepted the publish, so a failed attempt cannot leave the app
    /// believing a grant the daemon never took.
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
    /// connection is refused rather than applied to the new one's grants.
    generation: u64,
    /// The client the current bridge publishes to. `None` between a
    /// disconnect and the supervisor's replacement, and after a failed
    /// publish until then.
    client: Option<DaemonClient>,
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
    /// [`Fintwind::new`]; nothing is shared until the user asks for it.
    pub(super) fn start_browser_collaboration(&mut self, cx: &mut Context<Self>) {
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
        let task = cx.spawn(async move |this, cx| {
            while let Ok(wake) = wake_rx.recv().await {
                let alive = this
                    .update(cx, |this, cx| this.handle_browser_bridge_wake(wake, cx))
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        });
        self.browser_collaboration.task = task;
    }

    fn handle_browser_bridge_wake(&mut self, wake: BridgeWake, cx: &mut Context<Self>) -> bool {
        match wake {
            BridgeWake::Client { client, generation } => {
                self.rebind_browser_collaboration(client, generation, cx);
            }
            BridgeWake::Notification {
                generation,
                notification,
            } => {
                // A receipt from a connection that has since been replaced
                // describes a world that no longer exists. Applying it — a
                // rejection, a revocation, a disconnect — would reach grants
                // the current connection just published.
                if generation != self.browser_collaboration.generation {
                    return true;
                }
                match notification {
                    BrowserNotification::Request(request) => {
                        self.dispatch_browser_request(request, generation, cx);
                    }
                    BrowserNotification::Cancel(request_id) => {
                        self.cancel_browser_request(request_id, cx);
                    }
                    // The connection died. Grants go with it; the bridge
                    // thread waits for the supervisor's replacement, so this
                    // loop stays alive rather than treating a reconnect as
                    // shutdown.
                    BrowserNotification::Disconnected => {
                        self.drop_browser_connection(cx);
                    }
                    // The daemon refused one or more of this connection's
                    // published pages. Only a view whose current grant is
                    // exactly the refused scope is revoked, so a rejection
                    // that crossed a re-share cannot take the new grant.
                    BrowserNotification::ShareRejected { scopes, message } => {
                        let revoked = self.revoke_receipt_scopes(&scopes, cx);
                        if revoked {
                            self.show_toast(tr!(
                                "browser_collaboration.share_rejected",
                                message = message
                            ));
                        }
                    }
                    // The daemon retired scopes itself — most importantly
                    // because the session's runtime was replaced. The UI must
                    // not keep claiming a share the daemon no longer honors.
                    BrowserNotification::ScopesRevoked(scopes) => {
                        let revoked = self.revoke_receipt_scopes(&scopes, cx);
                        if revoked {
                            self.show_toast(tr!("browser_collaboration.runtime_replaced"));
                        }
                    }
                }
            }
        }
        true
    }

    /// Adopt a daemon client. A reconnect never inherits a grant: every
    /// share is revoked first and the previous connection's consumer is
    /// already retired by the bridge thread. The new connection is not
    /// re-published anything automatically.
    fn rebind_browser_collaboration(
        &mut self,
        client: DaemonClient,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        self.drop_browser_connection(cx);
        self.browser_collaboration.generation = generation;
        self.browser_collaboration.client = Some(client);
    }

    /// Drop the current connection's publishing ability and every grant.
    /// Used on disconnect, on a failed publish, and before a rebind.
    fn drop_browser_connection(&mut self, cx: &mut Context<Self>) {
        self.browser_collaboration.client = None;
        self.browser_collaboration.shares.clear();
        self.browser_collaboration.pending.clear();
        self.revoke_all_browser_shares(cx);
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
                this.handle_browser_collaboration_event(event, cx);
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
    }

    /// Drop one browser's subscription and publish the last set without it.
    /// Used where no `Context` is at hand (session removal); the cache is
    /// the last published snapshot, so this stays correct without entities.
    pub(super) fn retire_browser_collaboration(&mut self, browser_id: Uuid) {
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
        event: &BrowserCollaborationEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            BrowserCollaborationEvent::ShareChanged => self.publish_browser_shares(cx),
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
                        previous.scope == share.scope && previous.url == share.url
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
        self.browser_collaboration.client = None;
        self.browser_collaboration.shares.clear();
        self.revoke_all_browser_shares(cx);
        self.show_toast(tr!("browser_collaboration.publish_failed", error = error));
        cx.notify();
    }

    /// Apply the daemon's own revocation or rejection of scopes. A scope only
    /// bites when the view's current grant is exactly that scope, so a stale
    /// receipt cannot take a grant the user issued afterwards.
    fn revoke_receipt_scopes(&mut self, scopes: &[BrowserScope], cx: &mut Context<Self>) -> bool {
        let mut revoked_any = false;
        for scope in scopes {
            let Some(browser) = self.right_panel_browsers.get(&scope.page_id).cloned() else {
                continue;
            };
            let matches = browser
                .read(cx)
                .browser_share()
                .is_some_and(|share| share.scope == *scope);
            if matches {
                browser.update(cx, |view, cx| view.revoke_browser_share(cx));
                revoked_any = true;
            }
        }
        revoked_any
    }

    /// Route one daemon-delivered request to the page it names. A stale
    /// grant — wrong session, or a runtime that moved on — is refused and
    /// revoked rather than executed.
    fn dispatch_browser_request(
        &mut self,
        request: BrowserRequest,
        generation: u64,
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
        let scope = request.scope.clone();
        let runtime_current = self.state.selected_session == Some(scope.session_id)
            && self
                .selected_session()
                .and_then(|session| session.runtime_event_cursor)
                .is_some_and(|cursor| cursor.runtime_id == scope.runtime_id);
        if !runtime_current {
            // The page answers any approval it still holds when the grant is
            // revoked; the request itself is always answered, here.
            self.revoke_browser_grant(scope.page_id, cx);
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error(
                    "This page's session or runtime changed; share it again to continue.",
                ),
            );
            return;
        }
        let Some(browser) = self.right_panel_browsers.get(&scope.page_id).cloned() else {
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error("The shared page is no longer open."),
            );
            return;
        };
        let granted = browser
            .read(cx)
            .browser_share()
            .is_some_and(|share| share.scope == scope);
        if !granted {
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error("This page is not shared under the requested grant."),
            );
            return;
        }
        self.browser_collaboration
            .pending
            .insert(request.request_id, client);
        browser.update(cx, |view, cx| view.handle_browser_request(request, cx));
    }

    /// The daemon gave up on a request: retire it wherever it sits.
    fn cancel_browser_request(&mut self, request_id: Uuid, cx: &mut Context<Self>) {
        self.browser_collaboration.pending.remove(&request_id);
        for browser in self.right_panel_browsers.values() {
            browser.update(cx, |view, cx| view.cancel_browser_request(request_id, cx));
        }
    }

    /// Revoke every live grant. Used on session switches, tab and session
    /// removal, and daemon reconnects. Revoking runs first: a view whose
    /// approval was open answers its own request through the pending map
    /// before the leftovers are dropped. The share cache is only touched by
    /// the publish those revocations trigger, so an empty set is published.
    pub(super) fn revoke_all_browser_shares(&mut self, cx: &mut Context<Self>) {
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

    /// Revoke one page's grant, by page id.
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

    /// What the header's browser control should offer for `browser_id`.
    /// An existing grant can always be taken back, even once its runtime has
    /// ended; only a new share needs every precondition.
    pub(super) fn browser_share_affordance(
        &self,
        browser_id: Uuid,
        cx: &App,
    ) -> BrowserShareAffordance {
        if self
            .right_panel_browsers
            .get(&browser_id)
            .is_some_and(|browser| browser.read(cx).browser_share().is_some())
        {
            return BrowserShareAffordance::Revoke;
        }
        let shared_pages = self
            .right_panel_browsers
            .values()
            .filter(|browser| browser.read(cx).browser_share().is_some())
            .count();
        if shared_pages >= MAX_BROWSER_PAGES_PER_CONNECTION {
            return BrowserShareAffordance::Blocked(BrowserShareBlocker::TooManyPages);
        }
        if self.state.selected_session.is_none() {
            return BrowserShareAffordance::Blocked(BrowserShareBlocker::NoSession);
        }
        // No live connection means nothing to publish a grant on.
        if self
            .browser_collaboration
            .client
            .as_ref()
            .is_none_or(|client| client.is_disconnected())
        {
            return BrowserShareAffordance::Blocked(BrowserShareBlocker::NoConnection);
        }
        // A cursor alone is not a runtime: a settled session keeps its old
        // cursor, and binding a grant to it would address a dead runtime.
        let session_id = self.state.selected_session.unwrap_or_default();
        if !self.runtimes.contains_key(&session_id) {
            return BrowserShareAffordance::Blocked(BrowserShareBlocker::NoRuntime);
        }
        BrowserShareAffordance::Share
    }

    /// Share the active page with the selected session's live runtime. A
    /// fresh grant id per share: nothing is inherited across shares.
    pub(super) fn share_active_browser(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        if !self.runtimes.contains_key(&session_id) {
            return;
        }
        let Some(runtime_id) = self
            .selected_session()
            .and_then(|session| session.runtime_event_cursor)
            .map(|cursor| cursor.runtime_id)
        else {
            return;
        };
        let Some(browser_id) =
            self.active_right_panel_surface()
                .and_then(|surface| match surface {
                    RightPanelSurface::Browser(browser_id) => Some(*browser_id),
                    _ => None,
                })
        else {
            return;
        };
        // Refuse before the daemon has to: the per-connection page limit is
        // small and known, so the user is told instead of the grant silently
        // landing nowhere.
        let shared_pages = self
            .right_panel_browsers
            .values()
            .filter(|browser| browser.read(cx).browser_share().is_some())
            .count();
        if shared_pages >= MAX_BROWSER_PAGES_PER_CONNECTION {
            self.show_toast(tr!("browser_collaboration.too_many_pages"));
            cx.notify();
            return;
        }
        let Some(browser) = self.right_panel_browsers.get(&browser_id).cloned() else {
            return;
        };
        let scope = BrowserScope {
            session_id,
            runtime_id,
            page_id: browser_id,
            grant_id: Uuid::new_v4(),
        };
        browser.update(cx, |view, cx| view.begin_browser_share(scope, cx));
    }

    /// Take the active page back.
    pub(super) fn revoke_active_browser_share(&mut self, cx: &mut Context<Self>) {
        let Some(browser_id) =
            self.active_right_panel_surface()
                .and_then(|surface| match surface {
                    RightPanelSurface::Browser(browser_id) => Some(*browser_id),
                    _ => None,
                })
        else {
            return;
        };
        self.revoke_browser_grant(browser_id, cx);
    }
}
