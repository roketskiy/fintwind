//! Broker for the daemon side of the browser collaboration bridge.
//!
//! Failure modes this module exists to prevent, in the order they bite:
//!
//! - **Unauthorized execution**: a request for a page that was never
//!   published, a scope with nil ids, or a session whose runtime is not the
//!   active one is refused with an error. There is no global "current page"
//!   to fall back to.
//! - **Cross-session leakage**: every request carries the full scope
//!   (session, runtime, page, grant) and is matched exactly; a request
//!   addressed to one session can never be served by another session's page.
//! - **Disconnect and replay**: publications, pending requests and grants are
//!   live-only. Nothing is journalled as a driver event, so a reconnecting
//!   client replays nothing; a replaced runtime drops the old grants.
//! - **Wrong client answering**: only the connection that published a page
//!   may answer or cancel its requests, and an answer is only trusted while
//!   the grant is still current. Another connection cannot answer for it.
//! - **Timeout side effects**: a timeout cancels the GUI-side request and
//!   returns a terminal error. It is never retried, because the action may
//!   already have happened.
//! - **Replayed request ids**: requestor RPC ids are remembered per live
//!   connection in a never-evicted set, so a repeated id is refused instead
//!   of executed again. This is not a memo of outcomes: nothing caches what
//!   a page returned, and a repeated id always answers "observe again",
//!   never the earlier result.
//! - **Cancelled-before-start**: a cancel that arrives before its invoke has
//!   registered (the caller gave up immediately) is remembered as a
//!   tombstone, so the late invoke refuses instead of executing.
//!
//! All state transitions happen under one mutex. Channel sends are
//! non-blocking (unbounded crossbeam senders), so holding the lock across a
//! send cannot stall the daemon connection threads.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crossbeam_channel::{Sender, unbounded};
use parking_lot::Mutex;
use uuid::Uuid;

use fintwind_protocol::ServerMessage;
use fintwind_protocol::browser::{
    BROWSER_REQUEST_TIMEOUT_MS, BrowserAction, BrowserRequest, BrowserResult, BrowserScope,
    BrowserShare, MAX_BROWSER_PAGES_PER_CONNECTION, MAX_BROWSER_TITLE_BYTES,
};

/// Global bound on live publications, so a crowd of connections cannot grow
/// the broker without limit.
pub const MAX_BROWSER_PUBLICATIONS: usize = 256;
/// One in-flight action per scope: a second request for the same page queues
/// an error instead of racing the first one.
pub const MAX_BROWSER_PENDING_PER_SCOPE: usize = 1;
/// Global bound on in-flight actions.
pub const MAX_BROWSER_PENDING_REQUESTS: usize = 64;
/// Requestor RPC ids remembered per live connection. Ids are never evicted:
/// a full set rejects new requests from that connection until it closes, so
/// an id can never come back around for a second execution.
pub const MAX_BROWSER_SEEN_IDS_PER_CONNECTION: usize = 1024;
/// Global bound across connections for the same reason.
pub const MAX_BROWSER_SEEN_IDS_TOTAL: usize = 4096;
/// Cancel tombstones remembered per connection for cancels that arrived
/// before their invoke registered.
pub const MAX_BROWSER_TOMBSTONES_PER_CONNECTION: usize = 1024;
/// Global bound across connections. When either bound is reached the broker
/// fails closed: new cancels are refused loudly and new invokes are refused,
/// because a cancel that cannot be remembered must not let an action run.
pub const MAX_BROWSER_TOMBSTONES_TOTAL: usize = 4096;

const BROWSER_REQUEST_TIMEOUT: Duration = Duration::from_millis(BROWSER_REQUEST_TIMEOUT_MS);

struct Publication {
    share: BrowserShare,
    /// The server-side connection id that owns this page. Publish is bound
    /// to the connection, never to the client-chosen hello uuid.
    subscriber_id: u64,
    outgoing: Sender<ServerMessage>,
}

struct PendingRequest {
    /// The internal id the GUI sees; the invoking RPC keeps its own
    /// `request_id`, recorded below so results and cancels map back.
    request_id: Uuid,
    owner_subscriber_id: u64,
    owner_outgoing: Sender<ServerMessage>,
    requestor_subscriber_id: u64,
    requestor_request_id: Uuid,
    scope: BrowserScope,
    respond: Sender<BrowserResult>,
}

#[derive(Default)]
struct BrokerState {
    publications: Vec<Publication>,
    pending: Vec<PendingRequest>,
    /// session -> active runtime, mirrored from the server's runtime map.
    active_runtimes: HashMap<Uuid, Uuid>,
    /// Requestor RPC ids already taken by each live connection. This is not
    /// a result cache: it remembers which ids were spent so a replayed id is
    /// refused instead of executed again, and a full set stalls the
    /// connection rather than evicting an id.
    seen_request_ids: HashMap<u64, HashSet<Uuid>>,
    /// Cancels that arrived with no pending request to match. An invoke that
    /// later presents one of these ids refuses instead of executing, so a
    /// caller that gave up immediately cannot leave an action running.
    cancelled_rpc_ids: HashMap<u64, HashSet<Uuid>>,
    /// The latest outgoing sender per connection, used to deliver revocation
    /// notices to a page owner even after its pages are gone.
    outgoing_by_connection: HashMap<u64, Sender<ServerMessage>>,
}

pub(crate) struct BrowserBroker {
    state: Mutex<BrokerState>,
}

impl BrowserBroker {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(BrokerState::default()),
        }
    }

    /// Mirror a session runtime becoming active, exactly where the server
    /// calls `Hub::begin_runtime`. Publications left over from a previous
    /// runtime of the same session are revoked here, and their pending
    /// requests fail closed rather than executing against a page that now
    /// belongs to a different runtime.
    pub(crate) fn note_runtime(&self, session_id: Uuid, runtime_id: Uuid) {
        let mut state = self.state.lock();
        state.active_runtimes.insert(session_id, runtime_id);
        Self::purge_stale_scopes(&mut state, session_id, Some(runtime_id));
    }

    /// Mirror a session runtime ending, exactly where the server calls
    /// `Hub::end_runtime`: `runtime_id` of `None` forces the end (session
    /// removal), and a concrete id only acts when it is still the active one,
    /// so a late terminal command cannot purge a newer runtime's grants.
    pub(crate) fn forget_runtime(&self, session_id: Uuid, runtime_id: Option<Uuid>) {
        let mut state = self.state.lock();
        let matches_active = runtime_id
            .is_none_or(|runtime_id| state.active_runtimes.get(&session_id) == Some(&runtime_id));
        if !matches_active {
            return;
        }
        state.active_runtimes.remove(&session_id);
        Self::purge_stale_scopes(&mut state, session_id, None);
    }

    /// Revoke every scope of `session_id` that does not belong to
    /// `runtime_id` (`None` revokes the whole session), tell each affected
    /// owner once, and fail the session's in-flight requests. Scopes are
    /// matched exactly, so a later re-share under a new grant is a different
    /// scope and survives.
    fn purge_stale_scopes(state: &mut BrokerState, session_id: Uuid, runtime_id: Option<Uuid>) {
        let is_stale = |scope: &BrowserScope| {
            scope.session_id == session_id
                && !runtime_id.is_some_and(|runtime| runtime == scope.runtime_id)
        };
        // One revocation notice per owner, deduplicated: a scope that had a
        // page and an in-flight request is reported once.
        let mut revoked_per_owner: HashMap<u64, Vec<BrowserScope>> = HashMap::new();
        let mut kept = Vec::with_capacity(state.publications.len());
        for publication in std::mem::take(&mut state.publications) {
            if is_stale(&publication.share.scope) {
                revoked_per_owner
                    .entry(publication.subscriber_id)
                    .or_default()
                    .push(publication.share.scope);
            } else {
                kept.push(publication);
            }
        }
        state.publications = kept;
        let mut index = 0;
        while index < state.pending.len() {
            if is_stale(&state.pending[index].scope) {
                let pending = state.pending.swap_remove(index);
                // The GUI may still be acting on this scope, so it is told
                // to stop before the invoker learns the request failed.
                let _ = pending.owner_outgoing.send(ServerMessage::BrowserCancel {
                    request_id: pending.request_id,
                });
                let entry = revoked_per_owner
                    .entry(pending.owner_subscriber_id)
                    .or_default();
                if !entry.contains(&pending.scope) {
                    entry.push(pending.scope.clone());
                }
                let _ = pending.respond.send(BrowserResult::error(
                    "the session runtime ended or was replaced",
                ));
                continue;
            }
            index += 1;
        }
        for (owner_subscriber_id, scopes) in revoked_per_owner {
            if scopes.is_empty() {
                continue;
            }
            if let Some(outgoing) = state.outgoing_by_connection.get(&owner_subscriber_id) {
                let _ = outgoing.send(ServerMessage::BrowserScopesRevoked { scopes });
            }
        }
    }

    /// Replace the pages published by `subscriber_id`. Other connections'
    /// publications are untouched. All-or-nothing: any violation publishes
    /// nothing and leaves the previous set exactly as it was. Errors name
    /// ids only, never page content, so a rejected publish cannot leak what
    /// the page held.
    pub(crate) fn publish(
        &self,
        subscriber_id: u64,
        pages: Vec<BrowserShare>,
        outgoing: Sender<ServerMessage>,
    ) -> anyhow::Result<()> {
        if pages.len() > MAX_BROWSER_PAGES_PER_CONNECTION {
            anyhow::bail!(
                "a connection may publish at most {MAX_BROWSER_PAGES_PER_CONNECTION} browser pages"
            );
        }
        // Everything is validated before any mutation: a rejected publish
        // must not disturb the connection's current pages or grants.
        for page in &pages {
            if !page.scope.is_well_formed() {
                anyhow::bail!("a published browser scope has nil session, runtime, page or grant");
            }
            if page.title.len() > MAX_BROWSER_TITLE_BYTES {
                anyhow::bail!("a published browser title is too long");
            }
            // A shared page's URL is held to the same standard as a URL the
            // daemon would ask the GUI to navigate to.
            BrowserAction::validate_url(&page.url).map_err(anyhow::Error::msg)?;
        }
        for (index, page) in pages.iter().enumerate() {
            if pages[..index]
                .iter()
                .any(|earlier| earlier.scope.page_id == page.scope.page_id)
            {
                anyhow::bail!(
                    "browser page {} is published twice in one request",
                    page.scope.page_id
                );
            }
        }
        let mut state = self.state.lock();
        state
            .outgoing_by_connection
            .insert(subscriber_id, outgoing.clone());
        let other_publications = state
            .publications
            .iter()
            .filter(|publication| publication.subscriber_id != subscriber_id)
            .count();
        if other_publications + pages.len() > MAX_BROWSER_PUBLICATIONS {
            anyhow::bail!("too many browser pages are published");
        }
        for page in &pages {
            if state.publications.iter().any(|publication| {
                publication.subscriber_id != subscriber_id
                    && publication.share.scope.page_id == page.scope.page_id
            }) {
                anyhow::bail!(
                    "browser page {} is already published by another connection",
                    page.scope.page_id
                );
            }
        }
        for page in &pages {
            if state.active_runtimes.get(&page.scope.session_id) != Some(&page.scope.runtime_id) {
                anyhow::bail!(
                    "session {} has no live runtime {} to publish a browser page for",
                    page.scope.session_id,
                    page.scope.runtime_id
                );
            }
        }
        // The new set becomes this connection's current scopes. Anything
        // still pending for a scope it no longer covers is cancelled: the
        // GUI is told to stop acting, and the invoker gets an error.
        let current_scopes: Vec<BrowserScope> =
            pages.iter().map(|page| page.scope.clone()).collect();
        state
            .publications
            .retain(|publication| publication.subscriber_id != subscriber_id);
        for page in pages {
            state.publications.push(Publication {
                share: page,
                subscriber_id,
                outgoing: outgoing.clone(),
            });
        }
        Self::retire_uncovered_pending(&mut state, subscriber_id, &current_scopes);
        Ok(())
    }

    /// Cancel pending requests this connection owns whose scope is no longer
    /// covered by its current publications.
    fn retire_uncovered_pending(
        state: &mut BrokerState,
        subscriber_id: u64,
        current_scopes: &[BrowserScope],
    ) {
        let mut index = 0;
        while index < state.pending.len() {
            let pending = &state.pending[index];
            if pending.owner_subscriber_id == subscriber_id
                && !current_scopes.iter().any(|scope| *scope == pending.scope)
            {
                let pending = state.pending.swap_remove(index);
                let _ = pending.owner_outgoing.send(ServerMessage::BrowserCancel {
                    request_id: pending.request_id,
                });
                let _ = pending
                    .respond
                    .send(BrowserResult::error("the browser page share was revoked"));
                continue;
            }
            index += 1;
        }
    }

    /// Pages shared for `runtime_id` of `session_id`. Empty when that runtime
    /// is not the session's active one.
    pub(crate) fn list(&self, session_id: Uuid, runtime_id: Uuid) -> Vec<BrowserShare> {
        let state = self.state.lock();
        if state.active_runtimes.get(&session_id) != Some(&runtime_id) {
            return Vec::new();
        }
        state
            .publications
            .iter()
            .filter(|publication| {
                publication.share.scope.session_id == session_id
                    && publication.share.scope.runtime_id == runtime_id
            })
            .map(|publication| publication.share.clone())
            .collect()
    }

    /// Route one action to the GUI that owns the page and wait for its
    /// answer. Errors are [`BrowserResult::Error`], never a panic and never
    /// a retry.
    ///
    /// This is the full entry point: the action is validated here, the
    /// caller's cancel tombstones are consulted, and the requestor's RPC id
    /// is recorded before anything executes, so neither a replayed id nor a
    /// cancel that arrived early can cause an action to run.
    pub(crate) fn invoke(
        &self,
        scope: BrowserScope,
        action: BrowserAction,
        requestor_subscriber_id: u64,
        requestor_request_id: Uuid,
    ) -> BrowserResult {
        if !scope.is_well_formed() {
            return BrowserResult::error("the browser scope is not well formed");
        }
        // A nil request id is the fire-and-forget form; an action with side
        // effects never runs under one, because replay could not be refused.
        if requestor_request_id.is_nil() {
            return BrowserResult::error("a browser invoke requires a request id");
        }
        if let Err(message) = action.validate() {
            return BrowserResult::error(message);
        }
        let mut state = self.state.lock();
        // A cancel that arrived before this invoke registered wins: the
        // caller gave up, so the action must not run at all.
        if state
            .cancelled_rpc_ids
            .get(&requestor_subscriber_id)
            .is_some_and(|ids| ids.contains(&requestor_request_id))
        {
            return BrowserResult::error("this browser request was cancelled before it started");
        }
        // Tombstones are bounded; when the bound is reached no new cancel can
        // be remembered, so invokes fail closed instead of risking an
        // action whose cancel would be silently forgotten.
        if tombstones_total(&state) >= MAX_BROWSER_TOMBSTONES_TOTAL
            || state
                .cancelled_rpc_ids
                .get(&requestor_subscriber_id)
                .is_some_and(|ids| ids.len() >= MAX_BROWSER_TOMBSTONES_PER_CONNECTION)
        {
            return BrowserResult::error(
                "too many cancelled browser requests; reconnect to continue",
            );
        }
        // Spend the requestor's RPC id before executing. The set is never
        // evicted, so a repeat of a spent id fails closed; a full set
        // rejects new requests until the connection closes.
        let seen_total: usize = state.seen_request_ids.values().map(|ids| ids.len()).sum();
        if seen_total >= MAX_BROWSER_SEEN_IDS_TOTAL {
            return BrowserResult::error("too many browser requests have been made");
        }
        let seen = state
            .seen_request_ids
            .entry(requestor_subscriber_id)
            .or_default();
        if seen.len() >= MAX_BROWSER_SEEN_IDS_PER_CONNECTION {
            return BrowserResult::error(
                "too many browser requests from this connection; reconnect to continue",
            );
        }
        if !seen.insert(requestor_request_id) {
            return BrowserResult::error(
                "this browser request was already attempted; observe again instead of retrying",
            );
        }
        let Some(publication) = state
            .publications
            .iter()
            .find(|publication| publication.share.scope == scope)
        else {
            return BrowserResult::error("no live browser page is shared for this scope");
        };
        let owner_subscriber_id = publication.subscriber_id;
        let owner_outgoing = publication.outgoing.clone();
        if state.active_runtimes.get(&scope.session_id) != Some(&scope.runtime_id) {
            return BrowserResult::error("the session runtime is no longer active");
        }
        if state
            .pending
            .iter()
            .filter(|pending| pending.scope == scope)
            .count()
            >= MAX_BROWSER_PENDING_PER_SCOPE
        {
            return BrowserResult::error("a browser request is already pending for this scope");
        }
        if state.pending.len() >= MAX_BROWSER_PENDING_REQUESTS {
            return BrowserResult::error("too many browser requests are pending");
        }
        let request_id = Uuid::new_v4();
        let (respond, result_rx) = unbounded();
        let request = BrowserRequest {
            request_id,
            scope: scope.clone(),
            action,
        };
        // Send while still holding the lock. The send is non-blocking
        // (unbounded channel), and sending here guarantees the owner sees
        // the request before any later cancel can be recorded, so the GUI
        // can never observe a cancel for a request it has not received.
        if owner_outgoing
            .send(ServerMessage::BrowserRequest { request })
            .is_err()
        {
            return BrowserResult::error("the page owner connection is gone");
        }
        state.pending.push(PendingRequest {
            request_id,
            owner_subscriber_id,
            owner_outgoing: owner_outgoing.clone(),
            requestor_subscriber_id,
            requestor_request_id,
            scope,
            respond,
        });
        drop(state);
        match result_rx.recv_timeout(BROWSER_REQUEST_TIMEOUT) {
            Ok(result) => result,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                // The GUI action may still be running; the cancel is best
                // effort. The outcome is uncertain, so the caller must
                // re-observe instead of retrying.
                self.cancel_target(request_id);
                BrowserResult::error(format!(
                    "the browser request timed out after {BROWSER_REQUEST_TIMEOUT_MS} ms"
                ))
            }
            Err(_) => {
                self.drop_pending(request_id);
                BrowserResult::error("the browser request was abandoned")
            }
        }
    }

    /// Accept an answer from the owning connection. A late or unknown answer
    /// is acknowledged and ignored; an answer from any other connection is
    /// refused.
    pub(crate) fn complete(
        &self,
        request_id: Uuid,
        responder_subscriber_id: u64,
        result: BrowserResult,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        let Some(index) = state
            .pending
            .iter()
            .position(|pending| pending.request_id == request_id)
        else {
            return Ok(());
        };
        if state.pending[index].owner_subscriber_id != responder_subscriber_id {
            anyhow::bail!(
                "only the GUI connection that owns the page may answer this browser request"
            );
        }
        let pending = state.pending.swap_remove(index);
        // An answer is only trusted while the grant is still current: the
        // page must remain published by this owner under this exact scope,
        // and the runtime must still be active.
        let grant_current = state.publications.iter().any(|publication| {
            publication.subscriber_id == pending.owner_subscriber_id
                && publication.share.scope == pending.scope
        }) && state.active_runtimes.get(&pending.scope.session_id)
            == Some(&pending.scope.runtime_id);
        let result = enforce_result_bound(result);
        let _ = if grant_current {
            pending.respond.send(result)
        } else {
            pending.respond.send(BrowserResult::error(
                "the browser page share is no longer active",
            ))
        };
        Ok(())
    }

    /// Cancel a browser request. Two callers are legitimate: the owning
    /// connection, addressing the internal id it received, and the invoking
    /// connection, addressing the RPC id it sent. A requestor cancel also
    /// tells the owner to stop acting, then fails the invoker. Any other
    /// connection is refused.
    ///
    /// When no pending request matches, the id is remembered as a tombstone
    /// for the cancelling connection: an invoke that later presents it
    /// refuses, so a cancel that outraced its dispatch worker cannot leave an
    /// action running. Tombstones are bounded; when the bound is reached the
    /// cancel is refused loudly rather than silently forgotten.
    pub(crate) fn cancel(
        &self,
        request_id: Uuid,
        canceller_subscriber_id: u64,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        let matched = state.pending.iter().position(|pending| {
            (pending.request_id == request_id
                && pending.owner_subscriber_id == canceller_subscriber_id)
                || (pending.requestor_request_id == request_id
                    && pending.requestor_subscriber_id == canceller_subscriber_id)
        });
        let Some(index) = matched else {
            if tombstones_total(&state) >= MAX_BROWSER_TOMBSTONES_TOTAL {
                anyhow::bail!("too many cancelled browser requests");
            }
            let tombstones = state
                .cancelled_rpc_ids
                .entry(canceller_subscriber_id)
                .or_default();
            if tombstones.len() >= MAX_BROWSER_TOMBSTONES_PER_CONNECTION {
                anyhow::bail!("too many cancelled browser requests from this connection");
            }
            tombstones.insert(request_id);
            return Ok(());
        };
        let pending = &state.pending[index];
        let is_owner = pending.owner_subscriber_id == canceller_subscriber_id;
        let is_requestor = pending.requestor_subscriber_id == canceller_subscriber_id;
        if !is_owner && !is_requestor {
            anyhow::bail!(
                "only the browser page owner or the invoking client may cancel this browser request"
            );
        }
        // A requestor addressing its own RPC id must also stop the GUI.
        let stopped_gui = is_requestor && pending.requestor_request_id == request_id;
        let pending = state.pending.swap_remove(index);
        if stopped_gui {
            let _ = pending.owner_outgoing.send(ServerMessage::BrowserCancel {
                request_id: pending.request_id,
            });
        }
        let message = if stopped_gui {
            "the browser request was cancelled by its caller"
        } else {
            "the page owner cancelled the request"
        };
        let _ = pending.respond.send(BrowserResult::error(message));
        Ok(())
    }

    /// Drop every trace of a closing connection: its publications, its
    /// spent requestor ids, its cancel tombstones, plus pending requests it
    /// owned (answered as failed) or requested (cancelled on the owner so a
    /// GUI task does not outlive its caller).
    pub(crate) fn remove_connection(&self, subscriber_id: u64) {
        let mut state = self.state.lock();
        state
            .publications
            .retain(|publication| publication.subscriber_id != subscriber_id);
        state.seen_request_ids.remove(&subscriber_id);
        state.cancelled_rpc_ids.remove(&subscriber_id);
        state.outgoing_by_connection.remove(&subscriber_id);
        let mut index = 0;
        while index < state.pending.len() {
            let owns = state.pending[index].owner_subscriber_id == subscriber_id;
            let requested = state.pending[index].requestor_subscriber_id == subscriber_id;
            if !owns && !requested {
                index += 1;
                continue;
            }
            let pending = state.pending.swap_remove(index);
            if owns {
                let _ = pending
                    .respond
                    .send(BrowserResult::error("the page owner disconnected"));
            } else {
                let _ = pending.owner_outgoing.send(ServerMessage::BrowserCancel {
                    request_id: pending.request_id,
                });
            }
        }
    }

    fn drop_pending(&self, request_id: Uuid) {
        let mut state = self.state.lock();
        state
            .pending
            .retain(|pending| pending.request_id != request_id);
    }

    /// Remove a pending request and tell the owner to abandon it.
    fn cancel_target(&self, request_id: Uuid) {
        let mut state = self.state.lock();
        if let Some(index) = state
            .pending
            .iter()
            .position(|pending| pending.request_id == request_id)
        {
            let pending = state.pending.swap_remove(index);
            let _ = pending.owner_outgoing.send(ServerMessage::BrowserCancel {
                request_id: pending.request_id,
            });
        }
    }
}

fn tombstones_total(state: &BrokerState) -> usize {
    state.cancelled_rpc_ids.values().map(|ids| ids.len()).sum()
}

/// Cap the serialized result; a broken or hostile page must not push
/// unbounded bytes through the bridge. The replacement message never
/// includes the original content, so page text cannot leak through the error.
fn enforce_result_bound(result: BrowserResult) -> BrowserResult {
    match serde_json::to_vec(&result) {
        Ok(bytes) if bytes.len() <= fintwind_protocol::browser::MAX_BROWSER_RESULT_BYTES => result,
        _ => BrowserResult::error("the browser result exceeds the size limit"),
    }
}
