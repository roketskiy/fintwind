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
//! - **Wrong launcher**: opening a tab is a launcher-only action. It is
//!   routed exclusively to the one connection that registered a launcher for
//!   the live session runtime, never to a page grant and never to a launcher
//!   of another session. Two launchers for one session runtime are an
//!   ambiguity the broker refuses instead of guessing, and a launcher that
//!   disappears mid-request fails its pending open instead of letting it
//!   complete against a capability that no longer exists.
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
/// Global bound on registered launchers, one per live connection at most, so
/// a crowd of connections cannot grow the broker through launchers either.
pub const MAX_BROWSER_HOSTS: usize = MAX_BROWSER_PUBLICATIONS;
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

/// A connection-level launcher capability for one live session runtime. The
/// scope's page and grant ids are launcher identities, not a real tab: this
/// is deliberately not a `BrowserShare`, so a launcher can never be listed,
/// addressed by a page action, or mistaken for a shared page.
struct HostPublication {
    scope: BrowserScope,
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
    /// Whether this request is an `Open` routed to a launcher. Only an open
    /// is answered while a host — not a page publication — still covers its
    /// scope.
    opening: bool,
    respond: Sender<BrowserResult>,
}

#[derive(Default)]
struct BrokerState {
    connected_callers: HashSet<u64>,
    publications: Vec<Publication>,
    /// Registered launchers, at most one per connection. Never listed, never
    /// routed a page action: only `Open` resolves through here.
    hosts: Vec<HostPublication>,
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
    pub(crate) fn register_connection(&self, connection: u64) {
        self.state.lock().connected_callers.insert(connection);
    }
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
    /// scope and survives. Launchers are revoked the same way: a launcher of
    /// a replaced runtime loses its capability and its pending opens fail.
    fn purge_stale_scopes(state: &mut BrokerState, session_id: Uuid, runtime_id: Option<Uuid>) {
        let is_stale = |scope: &BrowserScope| {
            scope.session_id == session_id
                && !runtime_id.is_some_and(|runtime| runtime == scope.runtime_id)
        };
        // One revocation notice per owner, deduplicated: a scope that had a
        // page, a launcher and an in-flight request is reported once.
        let mut revoked_per_owner: HashMap<u64, Vec<BrowserScope>> = HashMap::new();
        let remember = |revoked: &mut HashMap<u64, Vec<BrowserScope>>,
                        owner_subscriber_id: u64,
                        scope: BrowserScope| {
            let entry = revoked.entry(owner_subscriber_id).or_default();
            if !entry.contains(&scope) {
                entry.push(scope);
            }
        };
        let mut kept = Vec::with_capacity(state.publications.len());
        for publication in std::mem::take(&mut state.publications) {
            if is_stale(&publication.share.scope) {
                remember(
                    &mut revoked_per_owner,
                    publication.subscriber_id,
                    publication.share.scope,
                );
            } else {
                kept.push(publication);
            }
        }
        state.publications = kept;
        let mut kept_hosts = Vec::with_capacity(state.hosts.len());
        for host in std::mem::take(&mut state.hosts) {
            if is_stale(&host.scope) {
                remember(&mut revoked_per_owner, host.subscriber_id, host.scope);
            } else {
                kept_hosts.push(host);
            }
        }
        state.hosts = kept_hosts;
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
        // The new set becomes this connection's current page scopes. Its
        // launcher scope — if it registered one — is covered too: publishing
        // pages must never cancel a pending open that was routed to the
        // launcher. Anything still pending for a scope this connection no
        // longer covers at all is cancelled: the GUI is told to stop acting,
        // and the invoker gets an error.
        let mut current_scopes: Vec<BrowserScope> =
            pages.iter().map(|page| page.scope.clone()).collect();
        if let Some(host) = state
            .hosts
            .iter()
            .find(|host| host.subscriber_id == subscriber_id)
        {
            current_scopes.push(host.scope.clone());
        }
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

    /// Register, replace or clear this connection's browser launcher
    /// capability.
    ///
    /// `scope` of `Some` installs (or replaces) the launcher for one live
    /// session runtime. The scope carries fresh page and grant ids that are
    /// launcher identities, never a real tab, so registering one neither
    /// creates nor touches a page publication. A registration that would
    /// leave two connections holding a launcher for the same session runtime
    /// is refused: the target of an open would be ambiguous, and this broker
    /// never guesses. A `None` scope clears the connection's launcher and
    /// fails its pending opens without disturbing its page publications.
    pub(crate) fn register_host(
        &self,
        subscriber_id: u64,
        scope: Option<BrowserScope>,
        outgoing: Sender<ServerMessage>,
    ) -> anyhow::Result<()> {
        let Some(scope) = scope else {
            let mut state = self.state.lock();
            let Some(index) = state
                .hosts
                .iter()
                .position(|host| host.subscriber_id == subscriber_id)
            else {
                return Ok(());
            };
            let removed = state.hosts.swap_remove(index);
            Self::cancel_host_pending(
                &mut state,
                &removed,
                "the browser launcher was unregistered",
            );
            return Ok(());
        };
        if !scope.is_well_formed() {
            anyhow::bail!("a browser launcher scope has nil session, runtime, page or grant");
        }
        let mut state = self.state.lock();
        // Record the sender before anything else: a later revocation must be
        // deliverable even to a connection whose launcher is already gone.
        state
            .outgoing_by_connection
            .insert(subscriber_id, outgoing.clone());
        if state.active_runtimes.get(&scope.session_id) != Some(&scope.runtime_id) {
            anyhow::bail!(
                "session {} has no live runtime {} to register a browser launcher for",
                scope.session_id,
                scope.runtime_id
            );
        }
        let same_runtime_elsewhere = state.hosts.iter().any(|host| {
            host.subscriber_id != subscriber_id
                && host.scope.session_id == scope.session_id
                && host.scope.runtime_id == scope.runtime_id
        });
        if same_runtime_elsewhere {
            anyhow::bail!(
                "another connection already registered a browser launcher for this session runtime"
            );
        }
        if !state
            .hosts
            .iter()
            .any(|host| host.subscriber_id == subscriber_id)
            && state.hosts.len() >= MAX_BROWSER_HOSTS
        {
            anyhow::bail!("too many browser launchers are registered");
        }
        // Replacing this connection's own launcher retires the old identity:
        // a pending open for it can no longer be answered, so it is failed
        // now instead of timing out.
        let replaced = state
            .hosts
            .iter()
            .position(|host| host.subscriber_id == subscriber_id)
            .map(|index| state.hosts.swap_remove(index));
        if let Some(previous) = replaced {
            if previous.scope != scope {
                Self::cancel_host_pending(
                    &mut state,
                    &previous,
                    "the browser launcher was replaced",
                );
            }
        }
        state.hosts.push(HostPublication {
            scope,
            subscriber_id,
            outgoing,
        });
        Ok(())
    }

    /// Fail every pending open routed to `host`, tell its owner to stop
    /// acting, and leave page publications untouched.
    fn cancel_host_pending(state: &mut BrokerState, host: &HostPublication, message: &str) {
        let mut index = 0;
        while index < state.pending.len() {
            let matches_host = state.pending[index].opening
                && state.pending[index].owner_subscriber_id == host.subscriber_id
                && state.pending[index].scope == host.scope;
            if matches_host {
                let pending = state.pending.swap_remove(index);
                let _ = pending.owner_outgoing.send(ServerMessage::BrowserCancel {
                    request_id: pending.request_id,
                });
                let _ = pending.respond.send(BrowserResult::error(message));
                continue;
            }
            index += 1;
        }
    }

    /// Pages shared for `runtime_id` of `session_id`. Empty when that runtime
    /// is not the session's active one.
    pub(crate) fn list(&self, session_id: Uuid, runtime_id: Uuid) -> Vec<BrowserShare> {
        let state = self.state.lock();
        list_shares(&state, session_id, runtime_id)
    }

    pub(crate) fn list_for_caller(
        &self,
        session_id: Uuid,
        runtime_id: Uuid,
        caller: u64,
    ) -> anyhow::Result<Vec<BrowserShare>> {
        let state = self.state.lock();
        if !state.connected_callers.contains(&caller) {
            anyhow::bail!("the browser caller disconnected before observation");
        }
        Ok(list_shares(&state, session_id, runtime_id))
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
        if !state.connected_callers.contains(&requestor_subscriber_id) {
            return BrowserResult::error("the browser caller disconnected before execution");
        }
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
        // `Open` is launcher-only: the lookup never consults page
        // publications, so a page grant cannot open a tab. Every other
        // action is page-only and never resolves through a launcher. The
        // scope match is exact and happens under this lock, so a launcher or
        // page that disappeared between an outer lookup and here fails
        // closed instead of being routed.
        let (owner_subscriber_id, owner_outgoing) = if matches!(action, BrowserAction::Open { .. })
        {
            let Some(host) = state.hosts.iter().find(|host| host.scope == scope) else {
                return BrowserResult::error(
                    "no live browser launcher is registered for this scope",
                );
            };
            (host.subscriber_id, host.outgoing.clone())
        } else {
            let Some(publication) = state
                .publications
                .iter()
                .find(|publication| publication.share.scope == scope)
            else {
                return BrowserResult::error("no live browser page is shared for this scope");
            };
            (publication.subscriber_id, publication.outgoing.clone())
        };
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
        let opening = matches!(action, BrowserAction::Open { .. });
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
            opening,
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
        // and the runtime must still be active. A pending open is current
        // only while this owner still holds the launcher for that scope —
        // a page publication never substitutes for a launcher.
        let grant_current = if pending.opening {
            state.hosts.iter().any(|host| {
                host.subscriber_id == pending.owner_subscriber_id && host.scope == pending.scope
            }) && state.active_runtimes.get(&pending.scope.session_id)
                == Some(&pending.scope.runtime_id)
        } else {
            state.publications.iter().any(|publication| {
                publication.subscriber_id == pending.owner_subscriber_id
                    && publication.share.scope == pending.scope
            }) && state.active_runtimes.get(&pending.scope.session_id)
                == Some(&pending.scope.runtime_id)
        };
        let result = enforce_result_bound(result);
        let stale_message = if pending.opening {
            "the browser launcher is no longer registered"
        } else {
            "the browser page share is no longer active"
        };
        let _ = if grant_current {
            pending.respond.send(result)
        } else {
            pending.respond.send(BrowserResult::error(stale_message))
        };
        Ok(())
    }

    /// Open a new tab through the one launcher registered for
    /// `session_id`/`runtime_id` and wait for the GUI's answer. The caller
    /// names a session and runtime, never a page: the launcher's own scope
    /// is the address, so a caller cannot aim this at another session's
    /// launcher or at a page grant.
    ///
    /// The lookups are deliberately split: the launcher is selected here,
    /// and the actual routing happens in [`Self::invoke`], which re-checks
    /// the caller, the live runtime and the exact launcher scope under its
    /// own lock. A launcher unregistered between the two steps therefore
    /// fails the call instead of opening a tab under a stale capability.
    pub(crate) fn open(
        &self,
        session_id: Uuid,
        runtime_id: Uuid,
        url: &str,
        requestor_subscriber_id: u64,
        requestor_request_id: Uuid,
    ) -> BrowserResult {
        if let Err(message) = BrowserAction::validate_url(url) {
            return BrowserResult::error(message);
        }
        let scope = {
            let state = self.state.lock();
            if !state.connected_callers.contains(&requestor_subscriber_id) {
                return BrowserResult::error("the browser caller disconnected before execution");
            }
            if state.active_runtimes.get(&session_id) != Some(&runtime_id) {
                return BrowserResult::error("the session runtime is no longer active");
            }
            let mut launchers = state
                .hosts
                .iter()
                .filter(|host| {
                    host.scope.session_id == session_id && host.scope.runtime_id == runtime_id
                })
                .map(|host| host.scope.clone());
            let Some(scope) = launchers.next() else {
                return BrowserResult::error(
                    "no live browser launcher is registered for this session runtime",
                );
            };
            if launchers.next().is_some() {
                // More than one launcher for one session runtime has no safe
                // answer, so none is chosen.
                return BrowserResult::error(
                    "more than one browser launcher is registered for this session runtime",
                );
            }
            scope
        };
        self.invoke(
            scope,
            BrowserAction::Open {
                url: url.to_owned(),
            },
            requestor_subscriber_id,
            requestor_request_id,
        )
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
    /// launchers, its spent requestor ids, its cancel tombstones, plus
    /// pending requests it owned (answered as failed) or requested
    /// (cancelled on the owner so a GUI task does not outlive its caller).
    pub(crate) fn remove_connection(&self, subscriber_id: u64) {
        let mut state = self.state.lock();
        state.connected_callers.remove(&subscriber_id);
        state
            .publications
            .retain(|publication| publication.subscriber_id != subscriber_id);
        // A closing connection's launchers die with it. Their pending opens
        // are failed by the pending loop below through the owner branch; the
        // launcher itself leaves no trace to replay against later.
        state
            .hosts
            .retain(|host| host.subscriber_id != subscriber_id);
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
                let _ = pending.respond.send(BrowserResult::error(
                    "the browser caller disconnected; an issued action may have occurred",
                ));
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

fn list_shares(state: &BrokerState, session_id: Uuid, runtime_id: Uuid) -> Vec<BrowserShare> {
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

fn tombstones_total(state: &BrokerState) -> usize {
    state.cancelled_rpc_ids.values().map(|ids| ids.len()).sum()
}

/// Cap the serialized result; a broken or hostile page must not push
/// unbounded bytes through the bridge. The replacement message never
/// includes the original content, so page text cannot leak through the error.
/// Media (a screenshot) is bounded by capture parameters rather than trust,
/// which is why it carries a larger, still-fixed budget.
fn enforce_result_bound(result: BrowserResult) -> BrowserResult {
    match serde_json::to_vec(&result) {
        Ok(bytes) if bytes.len() <= result.wire_budget() => result,
        _ => BrowserResult::error("the browser result exceeds the size limit"),
    }
}
