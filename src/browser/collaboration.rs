//! Explicit page capabilities and one-at-a-time, user-approved operations.
//!
//! Failure modes: stale grants, navigation during approval/execution, duplicate
//! requests, cancellation between CDP calls, ambiguous/hidden targets, unbounded
//! page data, and accidentally exposing arbitrary JavaScript or raw CDP.
//!
//! Two authorization shapes live here. The original is a manual, per-page share
//! the user confirms and every mutation on it asks again ([`begin_browser_share`]).
//! The second is *automatic*: full-access sessions get continuous automation on
//! a page ([`begin_browser_automation`] / [`set_browser_automatic`]). Automatic
//! authority is bound to a tab, so navigation keeps the scope and grant but
//! rotates the document guard — the old guard is invalidated the instant a new
//! document starts and a fresh one replaces it only once loading finishes, so a
//! stale element reference or an in-flight input step can never resume against a
//! document it did not observe.
//!
//! References follow the mature observe → reference → re-observe pattern rather
//! than copying any one framework: a snapshot runs in an isolated world, mints a
//! random nonce, stores each control's element object in a per-snapshot map
//! (never a DOM property), and returns `ref:<nonce>:<ordinal>` tokens. Actions
//! resolve a token through that map, refusing a token from an older snapshot or
//! a torn-down document with an explicit stale error, and still accept exact CSS
//! for callers that prefer it.
//!
//! [`begin_browser_share`]: BrowserView::begin_browser_share
//! [`begin_browser_automation`]: BrowserView::begin_browser_automation
//! [`set_browser_automatic`]: BrowserView::set_browser_automatic

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use futures_lite::FutureExt;
use gpui::{Context, EventEmitter, FocusHandle, IntoElement, Window, div, prelude::*, px};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{BrowserView, host::WebviewHost};
use crate::ui::ActivationExt;
use fintwind_protocol::browser::{
    BrowserAction, BrowserRequest, BrowserResult, BrowserScope, BrowserShare,
    MAX_BROWSER_MEDIA_BYTES,
};

/// Defensive cap on a single scroll gesture. The protocol already refuses a
/// zero or out-of-range delta; this bounds the value the native side sends even
/// if a caller skipped that check.
const MAX_SCROLL_DELTA: i32 = 2_000;

/// How long an automatic request waits for an in-flight document load to finish
/// before answering. Bounded so a stuck load cannot pin a request forever.
const AUTOMATION_LOAD_WAIT: Duration = Duration::from_secs(10);

pub(crate) enum BrowserCollaborationEvent {
    ShareRequested,
    ShareChanged,
    Finished {
        request_id: Uuid,
        result: BrowserResult,
    },
}

impl EventEmitter<BrowserCollaborationEvent> for BrowserView {}

#[derive(Default)]
pub(super) struct BrowserCollaboration {
    share: Option<BrowserShare>,
    valid: Option<Arc<AtomicBool>>,
    /// Native callbacks can invalidate a capability immediately, even while
    /// GPUI is borrowed and the entity update must wait one executor turn.
    pub(super) native_invalidation: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    /// An explicit stop during an automation launch. Replaced per launch so a
    /// finished launch's flag cannot cancel a newer one. Native focus and
    /// ordinary pointer input are deliberately not permission changes.
    pub(super) automation_intervention: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    /// Whether the current share is full-access automatic automation. Automatic
    /// shares skip per-action approval and keep their grant across navigation.
    automatic: bool,
    control_enabled: bool,
    pending: Option<BrowserRequest>,
    running: Option<(Uuid, Arc<AtomicBool>)>,
    controls: Option<[FocusHandle; 4]>,
    summary_scroll: gpui::ScrollHandle,
}

impl Drop for BrowserCollaboration {
    fn drop(&mut self) {
        if let Some(valid) = &self.valid {
            valid.store(false, Ordering::SeqCst);
        }
        if let Some((_, cancelled)) = &self.running {
            cancelled.store(true, Ordering::SeqCst);
        }
    }
}

/// The outcome of one poll inside the bounded "wait for the page to finish
/// loading" loop that serves an automatic request mid-navigation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitStep {
    Continue,
    Ready,
    Timeout,
    Cancelled,
}

impl BrowserView {
    pub(crate) fn browser_share(&self) -> Option<BrowserShare> {
        self.collaboration.share.clone()
    }

    /// Whether an explicit stop was requested during this automation launch.
    pub(super) fn automation_intervened(&self) -> bool {
        self.collaboration
            .automation_intervention
            .borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
    }

    /// The app pauses the launcher after an explicit stop so an agent cannot
    /// open a replacement tab. The flag survives revocation until a fresh
    /// authorized launch or manual share re-arms it.
    pub(crate) fn browser_automation_was_taken_over(&self) -> bool {
        self.automation_intervened()
    }

    /// Arm a fresh generational explicit-stop flag for a new launch.
    pub(super) fn arm_automation_intervention(&mut self) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        *self.collaboration.automation_intervention.borrow_mut() = Some(flag.clone());
        flag
    }

    #[cfg(feature = "browser-poc")]
    pub(crate) fn pending_browser_request(&self) -> Option<BrowserRequest> {
        self.collaboration.pending.clone()
    }

    /// The original manual share: per-page, per-mutation approval. Kept intact
    /// for the PoC and the manual path; nothing about it becomes automatic.
    pub(crate) fn begin_browser_share(&mut self, scope: BrowserScope, cx: &mut Context<Self>) {
        self.revoke_browser_share(cx);
        if !scope.is_well_formed() {
            return;
        }
        let Some(url) = self.current_url.as_deref() else {
            return;
        };
        if self.loading || self.host.is_none() || url.len() > 4096 || valid_url(url).is_err() {
            return;
        }
        self.collaboration.share = Some(BrowserShare {
            scope,
            url: url.to_owned(),
            title: self
                .page_title
                .as_deref()
                .unwrap_or_default()
                .chars()
                .take(256)
                .collect(),
        });
        let valid = Arc::new(AtomicBool::new(true));
        *self.collaboration.native_invalidation.borrow_mut() = Some(valid.clone());
        self.collaboration.valid = Some(valid);
        self.collaboration.automatic = false;
        // A manual share is a human action on the current page: it ends any
        // automation-open wait and drops any stale intervention flag.
        self.navigation_automation_pending = false;
        self.navigation_automation_cancelled = false;
        self.collaboration
            .automation_intervention
            .borrow_mut()
            .take();
        cx.emit(BrowserCollaborationEvent::ShareChanged);
        cx.notify();
    }

    /// Full-access automatic automation for the page this tab just opened. The
    /// caller has already confirmed full access and waited for a real, loaded
    /// page; this never changes keyboard ownership. Returns whether the page
    /// was actually shared: an explicit stop during load, a still-loading or
    /// invalid page, or a missing host
    /// all answer `false` and leave nothing shared.
    pub(crate) fn begin_browser_automation(
        &mut self,
        scope: BrowserScope,
        cx: &mut Context<Self>,
    ) -> bool {
        let ready = self.browser_ready_for_automation();
        self.revoke_browser_share(cx);
        if ready != Ok(true) {
            return false;
        }
        if self.automation_intervened() {
            return false;
        }
        if self.navigation_automation_cancelled {
            return false;
        }
        if !scope.is_well_formed() {
            return false;
        }
        let Some(url) = self.current_url.as_deref() else {
            return false;
        };
        if self.loading || self.host.is_none() || url.len() > 4096 || valid_url(url).is_err() {
            return false;
        }
        self.collaboration.share = Some(BrowserShare {
            scope,
            url: url.to_owned(),
            title: self
                .page_title
                .as_deref()
                .unwrap_or_default()
                .chars()
                .take(256)
                .collect(),
        });
        let valid = Arc::new(AtomicBool::new(true));
        *self.collaboration.native_invalidation.borrow_mut() = Some(valid.clone());
        self.collaboration.valid = Some(valid);
        self.collaboration.automatic = true;
        // The open resolved into a share, so its launch is over: drop the
        // intervention flag and the wait marker.
        self.collaboration
            .automation_intervention
            .borrow_mut()
            .take();
        self.navigation_automation_pending = false;
        self.navigation_automation_cancelled = false;
        cx.emit(BrowserCollaborationEvent::ShareChanged);
        cx.notify();
        true
    }

    /// Toggle full-access automation on an existing share. Promoting a manual
    /// share enables continuous automation; demoting must not silently retain
    /// the authority, so disabling revokes the share outright — a dropped mode
    /// or a supervised switch ends the grant rather than leaving it automatic.
    pub(crate) fn set_browser_automatic(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if !enabled {
            self.revoke_browser_share(cx);
            return;
        }
        // Only ever promote a share that already exists; never invent one.
        if self.collaboration.share.is_none() {
            return;
        }
        if !self.collaboration.automatic {
            self.collaboration.automatic = true;
            cx.emit(BrowserCollaborationEvent::ShareChanged);
            cx.notify();
        }
    }

    pub(crate) fn revoke_browser_share(&mut self, cx: &mut Context<Self>) {
        let changed = self.collaboration.share.take().is_some();
        self.collaboration.automatic = false;
        self.navigation_automation_pending = false;
        // An explicit-stop marker survives revocation so a queued ready timer
        // cannot share a cancelled opening. A fresh authorized launch or manual
        // share is the only reset; ordinary navigation is not a reset.
        self.collaboration.native_invalidation.borrow_mut().take();
        if let Some(valid) = self.collaboration.valid.take() {
            valid.store(false, Ordering::SeqCst);
        }
        if let Some(pending) = self.collaboration.pending.take() {
            self.finish_browser_request(
                pending.request_id,
                BrowserResult::Error {
                    message: "Page sharing was revoked; the operation was not approved.".into(),
                },
                cx,
            );
        }
        if let Some((id, cancelled)) = self.collaboration.running.take() {
            cancelled.store(true, Ordering::SeqCst);
            self.finish_browser_request(id, BrowserResult::Error { message: "Page sharing was revoked. An already-issued operation may have occurred; observe again, do not retry automatically.".into() }, cx);
        }
        if changed {
            cx.emit(BrowserCollaborationEvent::ShareChanged);
            cx.notify();
        }
    }

    pub(crate) fn take_over_browser(&mut self, cx: &mut Context<Self>) {
        let shared = self.collaboration.share.is_some();
        if self.navigation_automation_pending {
            self.navigation_automation_cancelled = true;
            if let Some(flag) = self.collaboration.automation_intervention.borrow().as_ref() {
                flag.store(true, Ordering::SeqCst);
            }
        }
        self.revoke_browser_share(cx);
        // A loading page may not have a share yet, but its launcher must still
        // stop. Emit even then so the app withdraws the open capability.
        if !shared {
            cx.emit(BrowserCollaborationEvent::ShareChanged);
            cx.notify();
        }
    }

    pub(crate) fn enable_browser_collaboration_control(
        &mut self,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.collaboration.control_enabled = enabled;
        cx.notify();
    }

    pub(crate) fn finish_browser_opening(&mut self, cx: &mut Context<Self>) {
        if self.navigation_automation_pending {
            self.navigation_automation_pending = false;
            cx.notify();
        }
    }

    /// A document transition is beginning (a real navigation, a reload, history,
    /// or an automation launch). A manual grant ends here; an automatic one
    /// keeps its scope and grant but its document guard is already invalidated
    /// natively, so we drop it here too and let the load events install a fresh
    /// one — a request landing now waits for the load instead of asking the user
    /// to re-share. Any still-open approval is retired.
    pub(super) fn note_document_unloading(&mut self, cx: &mut Context<Self>) {
        if self
            .collaboration
            .share
            .as_ref()
            .is_some_and(|_| self.collaboration.automatic)
        {
            if let Some(pending) = self.collaboration.pending.take() {
                self.finish_browser_request(
                    pending.request_id,
                    BrowserResult::error(
                        "The page navigated; the pending operation was not approved.",
                    ),
                    cx,
                );
            }
            if let Some(valid) = &self.collaboration.valid {
                valid.store(false, Ordering::SeqCst);
            }
        } else if self.collaboration.share.is_some() {
            // An unshared page may still be an authorized opening in progress.
            // Its first load must keep the explicit-stop toolbar available.
            self.revoke_browser_share(cx);
        }
    }

    /// A cross-document load finished successfully. For an automatic share this
    /// installs a *fresh* document guard (never reviving the retired one) and
    /// refreshes the published URL. Manual shares were revoked at navigation
    /// start and have nothing to do here.
    pub(super) fn note_document_ready(&mut self, url: &str, cx: &mut Context<Self>) {
        if !self.collaboration.automatic {
            return;
        }
        if let Some(share) = self.collaboration.share.as_mut() {
            share.url = url.chars().take(4096).collect();
        }
        let valid = Arc::new(AtomicBool::new(true));
        *self.collaboration.native_invalidation.borrow_mut() = Some(valid.clone());
        self.collaboration.valid = Some(valid);
        cx.emit(BrowserCollaborationEvent::ShareChanged);
    }

    /// A same-document navigation (a router pushing hash/state). An automatic
    /// share keeps its grant and refreshes its published URL, and rotates to a
    /// fresh guard — the document, and therefore its element references, is
    /// preserved, so a token from the prior snapshot still resolves to the same
    /// element. A manual grant is revoked as before.
    pub(super) fn note_source_changed(&mut self, url: &str, cx: &mut Context<Self>) {
        if self
            .collaboration
            .share
            .as_ref()
            .is_some_and(|_| self.collaboration.automatic)
        {
            if let Some(share) = self.collaboration.share.as_mut()
                && !url.is_empty()
            {
                share.url = url.chars().take(4096).collect();
            }
            let valid = Arc::new(AtomicBool::new(true));
            *self.collaboration.native_invalidation.borrow_mut() = Some(valid.clone());
            self.collaboration.valid = Some(valid);
            cx.emit(BrowserCollaborationEvent::ShareChanged);
        } else if self.collaboration.share.is_some() {
            self.revoke_browser_share(cx);
        }
    }

    pub(crate) fn handle_browser_request(
        &mut self,
        request: BrowserRequest,
        cx: &mut Context<Self>,
    ) {
        // Opening or closing a page is a launcher capability, never a page
        // grant: the app host routes `Open` and owns the surface's lifetime,
        // so a page-level grant must refuse both rather than act beyond the
        // page it was granted.
        let refusal = match &request.action {
            BrowserAction::Open { .. } => {
                Some("Opening a page is handled by the app host, not by this page's grant.")
            }
            BrowserAction::Close => {
                Some("Closing a page is handled by the app host, not by this page's grant.")
            }
            _ => None,
        };
        if let Some(message) = refusal {
            self.finish_browser_request(request.request_id, BrowserResult::error(message), cx);
            return;
        }
        let error = if self.collaboration.share.as_ref().map(|share| &share.scope)
            != Some(&request.scope)
        {
            Some("Page is not shared with this session and runtime.".to_owned())
        } else if self.collaboration.pending.is_some() || self.collaboration.running.is_some() {
            Some(
                "A browser request is already pending; observe again after it completes."
                    .to_owned(),
            )
        } else {
            request.action.validate().err()
        };
        if let Some(message) = error {
            self.finish_browser_request(request.request_id, BrowserResult::Error { message }, cx);
            return;
        }
        // Full access runs continuously: only a manual, non-automatic share
        // still stops for per-mutation approval. Observation is always allowed.
        let needs_approval = request.action.requires_approval() && !self.collaboration.automatic;
        if needs_approval {
            self.collaboration
                .summary_scroll
                .set_offset(gpui::Point::default());
            self.collaboration.pending = Some(request);
            cx.notify();
        } else {
            self.start_browser_request(request, cx);
        }
    }

    pub(crate) fn approve_browser_request(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self
            .collaboration
            .pending
            .as_ref()
            .is_some_and(|request| request.request_id == id)
        {
            let request = self.collaboration.pending.take().unwrap();
            self.start_browser_request(request, cx);
            cx.notify();
        }
    }

    pub(crate) fn reject_browser_request(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self
            .collaboration
            .pending
            .as_ref()
            .is_some_and(|request| request.request_id == id)
        {
            self.collaboration.pending.take();
            self.finish_browser_request(
                id,
                BrowserResult::Error {
                    message: "User rejected this browser operation.".into(),
                },
                cx,
            );
            cx.notify();
        }
    }

    pub(crate) fn cancel_browser_request(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self
            .collaboration
            .pending
            .as_ref()
            .is_some_and(|request| request.request_id == id)
        {
            self.collaboration.pending.take();
            cx.notify();
        }
        if self
            .collaboration
            .running
            .as_ref()
            .is_some_and(|(running, _)| *running == id)
        {
            if let Some((_, cancelled)) = self.collaboration.running.take() {
                cancelled.store(true, Ordering::SeqCst);
                cx.notify();
            }
        }
    }

    fn finish_browser_request(
        &mut self,
        request_id: Uuid,
        result: BrowserResult,
        cx: &mut Context<Self>,
    ) {
        cx.emit(BrowserCollaborationEvent::Finished { request_id, result });
    }

    fn start_browser_request(&mut self, request: BrowserRequest, cx: &mut Context<Self>) {
        if self.collaboration.share.as_ref().map(|share| &share.scope) != Some(&request.scope) {
            self.finish_browser_request(
                request.request_id,
                BrowserResult::Error {
                    message: "Browser authorization expired before execution.".into(),
                },
                cx,
            );
            return;
        }
        if self.host.is_none() {
            self.finish_browser_request(
                request.request_id,
                BrowserResult::Error {
                    message: "Native page is unavailable.".into(),
                },
                cx,
            );
            return;
        }
        // A full-access share crossed a navigation: wait — bounded and on the
        // foreground executor, never heavy UI or a sync block — for the load to
        // finish, then bind the *current* document guard rather than making the
        // user re-share. This is the one place the tab's ability to act
        // continuously across navigation is honored.
        if self.loading && self.collaboration.automatic {
            self.wait_for_page_ready(request, cx);
            return;
        }
        self.bind_and_execute(request, Arc::new(AtomicBool::new(false)), cx);
    }

    /// Bind the currently valid document guard and run the request on the
    /// background executor. The guard is captured here, so a document change
    /// mid-operation stops every later step of this one operation.
    fn bind_and_execute(
        &mut self,
        request: BrowserRequest,
        cancelled: Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) {
        if self.collaboration.share.as_ref().map(|share| &share.scope) != Some(&request.scope) {
            self.finish_browser_request(
                request.request_id,
                BrowserResult::Error {
                    message: "Browser authorization expired before execution.".into(),
                },
                cx,
            );
            return;
        }
        let Some(host) = self.host.clone() else {
            self.finish_browser_request(
                request.request_id,
                BrowserResult::Error {
                    message: "Native page is unavailable.".into(),
                },
                cx,
            );
            return;
        };
        // Only reachable for a manual share (automatic requests wait above): a
        // live manual grant with an in-flight load means the document is
        // mid-transition, which is refused rather than operated against.
        if self.loading {
            self.finish_browser_request(
                request.request_id,
                BrowserResult::Error {
                    message: "The page is still loading; take a fresh snapshot once it settles."
                        .into(),
                },
                cx,
            );
            return;
        }
        let Some(valid) = self
            .collaboration
            .valid
            .as_ref()
            .filter(|valid| valid.load(Ordering::SeqCst))
            .cloned()
        else {
            self.revoke_browser_share(cx);
            self.finish_browser_request(
                request.request_id,
                BrowserResult::Error {
                    message: "The page document changed before execution.".into(),
                },
                cx,
            );
            return;
        };
        self.collaboration.running = Some((request.request_id, cancelled.clone()));
        let guard = OperationGuard { valid, cancelled };
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = execute(host, request.action, &guard, &executor).await;
            let _ = this.update(cx, |this, cx| {
                // A revoked/cancelled request was already retired. Its late
                // completion must not become a success or replace a new grant.
                if this
                    .collaboration
                    .running
                    .as_ref()
                    .is_some_and(|(id, _)| *id == request.request_id)
                {
                    this.collaboration.running.take();
                    this.finish_browser_request(
                        request.request_id,
                        match result {
                            Ok(outcome) => outcome.into_result(),
                            Err(message) => BrowserResult::Error { message },
                        },
                        cx,
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Serve an automatic request that arrived mid-navigation. Polls a bounded
    /// 75 ms on the foreground executor (never a sync block), stops early on
    /// cancel, revoke, or human takeover, and re-enters execution with the fresh
    /// guard the load installed once the page settles.
    fn wait_for_page_ready(&mut self, request: BrowserRequest, cx: &mut Context<Self>) {
        let cancelled = Arc::new(AtomicBool::new(false));
        self.collaboration.running = Some((request.request_id, cancelled.clone()));
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let deadline = Instant::now() + AUTOMATION_LOAD_WAIT;
            loop {
                executor.timer(Duration::from_millis(75)).await;
                let decision = this.update(cx, |this, _cx| {
                    evaluate_wait(
                        this,
                        &request.scope,
                        request.request_id,
                        &cancelled,
                        deadline,
                    )
                });
                match decision.unwrap_or(WaitStep::Cancelled) {
                    WaitStep::Continue => continue,
                    WaitStep::Ready => {
                        let _ = this.update(cx, |this, cx| {
                            this.bind_and_execute(request.clone(), cancelled.clone(), cx);
                        });
                        return;
                    }
                    WaitStep::Timeout => {
                        let _ = this.update(cx, |this, cx| {
                            if this
                                .collaboration
                                .running
                                .as_ref()
                                .is_some_and(|(id, _)| *id == request.request_id)
                            {
                                this.collaboration.running.take();
                                this.finish_browser_request(
                                    request.request_id,
                                    BrowserResult::error(
                                        "The page is still loading; take a fresh snapshot once it settles.",
                                    ),
                                    cx,
                                );
                                cx.notify();
                            }
                        });
                        return;
                    }
                    WaitStep::Cancelled => return,
                }
            }
        })
        .detach();
    }

    pub(super) fn render_collaboration_control(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let shared = self.collaboration.share.is_some();
        let opening = self.navigation_automation_pending;
        if !self.collaboration.control_enabled && !shared && !opening {
            return None;
        }
        let theme = crate::theme::Theme::current(cx);
        let controls = self.collaboration_controls(cx);
        let label = if shared {
            tr!("browser_collaboration.shared")
        } else if opening {
            tr!("browser_collaboration.opening")
        } else {
            tr!("browser_collaboration.share")
        };
        let tooltip = if opening {
            tr!("browser_collaboration.stop_opening")
        } else if !shared {
            tr!("right_panel.share_browser")
        } else if self.collaboration.automatic {
            tr!("browser_collaboration.automatic_status")
        } else {
            tr!("browser_collaboration.shared_status")
        };
        let enabled = self.is_collaboration_action_enabled();
        let control = div()
            .id("browser-share-control")
            .track_focus(&controls[2])
            .tab_index(0)
            .tab_stop(enabled)
            .h_7()
            .px_2()
            .ml_1()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .focus_visible(|style| style.border_color(theme.accent))
            .flex_none()
            .flex()
            .items_center()
            .cursor_default()
            .text_size(crate::theme::ui_px(12.0))
            .text_color(if shared || opening {
                theme.text
            } else {
                theme.text_secondary
            })
            .when(shared || opening, |element| element.bg(theme.overlay))
            .child(
                gpui::svg()
                    .path(if shared || opening {
                        "icons/x.svg"
                    } else {
                        "icons/bot.svg"
                    })
                    .size_3()
                    .flex_none()
                    .text_color(theme.text_secondary),
            )
            .child(label)
            .tooltip(move |window, cx| {
                crate::ui::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            });
        Some(if enabled {
            control
                .hover(|element| element.bg(theme.overlay_strong))
                .active(|element| element.bg(theme.overlay_strong))
                .on_activation(cx, |this, _, cx| {
                    if this.collaboration.share.is_some() || this.navigation_automation_pending {
                        this.take_over_browser(cx);
                    } else {
                        cx.emit(BrowserCollaborationEvent::ShareRequested);
                    }
                })
        } else {
            control.opacity(0.55)
        })
    }

    fn collaboration_controls(&mut self, cx: &mut Context<Self>) -> [FocusHandle; 4] {
        self.collaboration
            .controls
            .get_or_insert_with(|| std::array::from_fn(|_| cx.focus_handle()))
            .clone()
    }

    fn focus_collaboration_control(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(controls) = &self.collaboration.controls
            && self.is_collaboration_action_enabled()
        {
            window.focus(&controls[2], cx);
        } else {
            window.focus(&self.focus_handle, cx);
        }
    }

    fn is_collaboration_action_enabled(&self) -> bool {
        self.collaboration.share.is_some()
            || self.navigation_automation_pending
            || (self.navigation_requested
                && !self.loading
                && self.host.is_some()
                && self.host_error.is_none()
                && self.navigation_error.is_none())
    }

    pub(super) fn render_collaboration_bar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        // Continuous automation has one compact toolbar control. A second row
        // is reserved for a real supervised decision, never for idle status.
        let pending = self.collaboration.pending.clone()?;
        let theme = crate::theme::Theme::current(cx);
        let controls = self.collaboration_controls(cx);
        let summary = match &pending.action {
            BrowserAction::Click { selector } => {
                tr!("browser_collaboration.click_summary", selector = selector)
            }
            BrowserAction::Fill { selector, text } => tr!(
                "browser_collaboration.fill_summary",
                selector = selector,
                text = text
            ),
            BrowserAction::Navigate { url } => {
                tr!("browser_collaboration.navigate_summary", url = url)
            }
            BrowserAction::Scroll { delta_y } => {
                tr!("browser_collaboration.scroll_summary", delta_y = *delta_y)
            }
            BrowserAction::Screenshot { .. } => {
                tr!("browser_collaboration.screenshot_summary")
            }
            BrowserAction::Evaluate { expression } => {
                // The approval row shows a bounded prefix of the expression,
                // never the whole script the page is about to run.
                let mut preview: String = expression.chars().take(64).collect();
                if expression.chars().nth(64).is_some() {
                    preview.push('…');
                }
                tr!(
                    "browser_collaboration.evaluate_summary",
                    expression = preview
                )
            }
            BrowserAction::ClickAt { x, y } => {
                tr!("browser_collaboration.click_at_summary", x = *x, y = *y)
            }
            BrowserAction::DoubleClick { selector } => {
                tr!(
                    "browser_collaboration.double_click_summary",
                    selector = selector
                )
            }
            BrowserAction::Press { selector, key } => {
                tr!(
                    "browser_collaboration.press_summary",
                    selector = selector,
                    key = key
                )
            }
            BrowserAction::Hover { selector } => {
                tr!("browser_collaboration.hover_summary", selector = selector)
            }
            BrowserAction::Select { selector, value } => {
                tr!(
                    "browser_collaboration.select_summary",
                    selector = selector,
                    value = value
                )
            }
            BrowserAction::Drag { from, to } => {
                tr!("browser_collaboration.drag_summary", from = from, to = to)
            }
            // `Open` and `Close` are refused before they are ever queued, so
            // neither has a pending row; finite strings keep the match
            // exhaustive.
            BrowserAction::Open { .. } => tr!("browser_collaboration.open_handled"),
            BrowserAction::Close => tr!("browser_collaboration.close_handled"),
            BrowserAction::Snapshot => tr!("browser_collaboration.reading"),
        };
        Some(
            div()
                .id("browser-collaboration-bar")
                .flex_none()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.surface)
                .text_color(theme.text)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape"
                        && !event.keystroke.modifiers.modified()
                        && let Some(request) = this.collaboration.pending.as_ref()
                    {
                        this.reject_browser_request(request.request_id, cx);
                        this.focus_collaboration_control(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .id("browser-operation-summary")
                        .track_focus(&controls[3])
                        .tab_index(0)
                        .border_1()
                        .border_color(theme.surface)
                        .focus_visible(|style| style.border_color(theme.accent))
                        .max_h_24()
                        .overflow_y_scroll()
                        .track_scroll(&self.collaboration.summary_scroll)
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                            if event.keystroke.modifiers.modified() {
                                return;
                            }
                            let scroll = &this.collaboration.summary_scroll;
                            let current = scroll.offset();
                            let limit = scroll.max_offset().y;
                            let next = match event.keystroke.key.as_str() {
                                "up" => current.y + px(20.0),
                                "down" => current.y - px(20.0),
                                "pageup" => current.y + px(80.0),
                                "pagedown" => current.y - px(80.0),
                                "home" => px(0.0),
                                "end" => -limit,
                                _ => return,
                            };
                            scroll.set_offset(gpui::point(current.x, next.clamp(-limit, px(0.0))));
                            cx.stop_propagation();
                            cx.notify();
                        }))
                        .text_size(crate::theme::ui_px(12.0))
                        .text_color(theme.text)
                        .child(summary),
                )
                .child(div().flex().flex_wrap().gap_2().map(|row| {
                    let id = pending.request_id;
                    row.child(
                        div()
                            .id("browser-approve")
                            .track_focus(&controls[0])
                            .tab_index(0)
                            .min_h_8()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_size(crate::theme::ui_px(12.0))
                            .border_1()
                            .border_color(theme.border)
                            .focus_visible(|style| style.border_color(theme.accent))
                            .cursor_default()
                            .hover(|style| style.bg(theme.overlay))
                            .active(|style| style.bg(theme.overlay_strong))
                            .child(tr!("browser_collaboration.approve_once"))
                            .on_activation(cx, move |this, window, cx| {
                                this.approve_browser_request(id, cx);
                                this.focus_collaboration_control(window, cx);
                            }),
                    )
                    .child(
                        div()
                            .id("browser-reject")
                            .track_focus(&controls[1])
                            .tab_index(0)
                            .min_h_8()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_size(crate::theme::ui_px(12.0))
                            .border_1()
                            .border_color(theme.border)
                            .focus_visible(|style| style.border_color(theme.accent))
                            .cursor_default()
                            .hover(|style| style.bg(theme.overlay))
                            .active(|style| style.bg(theme.overlay_strong))
                            .child(tr!("browser_collaboration.reject"))
                            .on_activation(cx, move |this, window, cx| {
                                this.reject_browser_request(id, cx);
                                this.focus_collaboration_control(window, cx);
                            }),
                    )
                })),
        )
    }
}

/// Pure decision for the bounded load-wait loop. Reads only in-memory view
/// state — never I/O — so it is safe to call from a foreground entity update.
fn evaluate_wait(
    this: &BrowserView,
    scope: &BrowserScope,
    _id: Uuid,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> WaitStep {
    if cancelled.load(Ordering::SeqCst) {
        return WaitStep::Cancelled;
    }
    if this.navigation_automation_cancelled {
        return WaitStep::Cancelled;
    }
    if this.collaboration.share.as_ref().map(|share| &share.scope) != Some(scope) {
        return WaitStep::Cancelled;
    }
    if !this.loading {
        return WaitStep::Ready;
    }
    if Instant::now() >= deadline {
        return WaitStep::Timeout;
    }
    WaitStep::Continue
}

struct OperationGuard {
    valid: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}
impl OperationGuard {
    fn check(&self) -> Result<(), String> {
        if self.valid.load(Ordering::SeqCst) && !self.cancelled.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err("Browser authorization was cancelled. An issued operation may have occurred; do not retry automatically.".into())
        }
    }

    /// A terminal dispatch may itself replace the document (navigation or a
    /// clicked link). Retiring that document is not a failed dispatch. This
    /// acknowledges only that input was issued, never that the next page loaded;
    /// explicit cancellation still reports an uncertain outcome.
    fn check_issued(&self) -> Result<(), String> {
        if !self.cancelled.load(Ordering::SeqCst) {
            Ok(())
        } else {
            self.check()
        }
    }
}

#[derive(Clone, Copy)]
enum CompletionGuard {
    SameDocument,
    Issued,
}

impl CompletionGuard {
    fn check(self, guard: &OperationGuard) -> Result<(), String> {
        match self {
            Self::SameDocument => guard.check(),
            Self::Issued => guard.check_issued(),
        }
    }
}

fn valid_url(raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|_| "Invalid browser URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(
            "Only HTTP(S) pages without URL credentials may be shared or navigated.".into(),
        );
    }
    Ok(url)
}

async fn cdp(
    host: &WebviewHost,
    method: &str,
    parameters: Value,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
) -> Result<Value, String> {
    cdp_with_completion(
        host,
        method,
        parameters,
        guard,
        executor,
        CompletionGuard::SameDocument,
    )
    .await
}

async fn cdp_with_completion(
    host: &WebviewHost,
    method: &str,
    parameters: Value,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
    completion: CompletionGuard,
) -> Result<Value, String> {
    guard.check()?;
    let parameters = executor.spawn(async move { parameters.to_string() }).await;
    guard.check()?;
    if !host.collaboration_visible() {
        return Err(
            "The shared page is not visible; return to it before requesting an operation.".into(),
        );
    }
    let receiver = host.webview.call_cdp(method, &parameters);
    let raw = async {
        receiver
            .recv()
            .await
            .map_err(|_| "Native browser disconnected".to_owned())?
    }
    .or(async {
        executor.timer(Duration::from_secs(3)).await;
        Err(format!(
            "Browser operation {method} timed out; observe again rather than retrying a mutation."
        ))
    })
    .await?;
    if raw.len() > 128 * 1024 {
        return Err("Native browser result exceeded the size limit".into());
    }
    let value = executor
        .spawn(async move {
            serde_json::from_str::<Value>(&raw)
                .map_err(|_| "Invalid native browser result".to_owned())
        })
        .await?;
    completion.check(guard)?;
    // A protocol-level refusal — a destroyed execution context above all —
    // must not fall through to the callers' missing-field errors, which
    // would blame the page's structure for a transport condition.
    if value.get("error").is_some() {
        return Err(
            "The browser refused the operation; take a fresh snapshot and observe again.".into(),
        );
    }
    Ok(value)
}

/// One `Page.captureScreenshot` with its own envelope. A capture is pure
/// observation, but its base64 payload is inherently larger than any
/// structured result, so it carries a wider timeout and size guard than the
/// shared CDP path instead of widening those for everything. Returns the
/// base64 image data.
async fn cdp_capture(
    host: &WebviewHost,
    parameters: Value,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
) -> Result<String, String> {
    guard.check()?;
    let parameters = executor.spawn(async move { parameters.to_string() }).await;
    guard.check()?;
    if !host.collaboration_visible() {
        return Err(
            "The shared page is not visible; return to it before requesting an operation.".into(),
        );
    }
    let receiver = host.webview.call_cdp("Page.captureScreenshot", &parameters);
    let raw = async {
        receiver
            .recv()
            .await
            .map_err(|_| "Native browser disconnected".to_owned())?
    }
    .or(async {
        executor.timer(Duration::from_secs(10)).await;
        Err("Browser screenshot timed out; observe the page again rather than retrying.".to_owned())
    })
    .await?;
    if raw.len() > 6 * 1024 * 1024 {
        return Err("The page screenshot exceeded the size limit.".into());
    }
    let value = executor
        .spawn(async move {
            serde_json::from_str::<Value>(&raw)
                .map_err(|_| "Invalid native browser result".to_owned())
        })
        .await?;
    if value.get("error").is_some() {
        return Err(
            "The browser refused the screenshot; take a fresh snapshot and observe again.".into(),
        );
    }
    let data = value
        .pointer("/data")
        .and_then(Value::as_str)
        .ok_or("Native page did not return a screenshot")?
        .to_owned();
    // A capture is observation, but it still races a document change: the
    // same-document check applies, so a capture of a replaced page is not
    // reported as success.
    guard.check()?;
    Ok(data)
}

/// Shared body for `Runtime.evaluate`. `await_promise` lets an action run an
/// async expression (a scroll-into-view that waits a frame to settle). A real
/// page exception is reported as a controlled failure — the target state may be
/// stale — rather than echoing the page's private exception body.
async fn evaluate_context(
    host: &WebviewHost,
    context: i64,
    expression: String,
    await_promise: bool,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
) -> Result<Value, String> {
    let mut parameters = json!({"contextId":context,"expression":expression,"returnByValue":true});
    if await_promise {
        parameters["awaitPromise"] = json!(true);
    }
    let value = cdp(host, "Runtime.evaluate", parameters, guard, executor).await?;
    if value.get("exceptionDetails").is_some() {
        return Err(
            "The page rejected the operation; the target state may be stale — take a fresh snapshot and observe again.".into(),
        );
    }
    let remote = value
        .pointer("/result")
        .cloned()
        .ok_or_else(|| "Native page did not return a structured result".to_owned())?;
    // `undefined` — and unserializable kinds such as functions — legitimately
    // carry no `value` field; they report as null instead of failing as a
    // native defect.
    Ok(remote.get("value").cloned().unwrap_or(Value::Null))
}

async fn evaluate(
    host: &WebviewHost,
    context: i64,
    expression: String,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
) -> Result<Value, String> {
    evaluate_context(host, context, expression, false, guard, executor).await
}

async fn evaluate_promise(
    host: &WebviewHost,
    context: i64,
    expression: String,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
) -> Result<Value, String> {
    evaluate_context(host, context, expression, true, guard, executor).await
}

/// Wait for down to complete before issuing up: WebView2 CDP commands may be
/// processed out of order. Once down has been issued, up is mandatory cleanup,
/// even after cancellation, so a request never leaves a key or button pressed.
async fn input_pair(
    host: &WebviewHost,
    method: &str,
    down: Value,
    up: Value,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
    completion: CompletionGuard,
) -> Result<(), String> {
    let (down, up) = executor
        .spawn(async move { (down.to_string(), up.to_string()) })
        .await;
    guard.check()?;
    if !host.collaboration_visible() {
        return Err("The shared page is not visible.".into());
    }
    let down = host.webview.call_cdp(method, &down);
    let wait = |receiver: smol::channel::Receiver<Result<String, String>>| async move {
        async {
            receiver
                .recv()
                .await
                .map_err(|_| "Native browser disconnected".to_owned())?
        }
        .or(async {
            executor.timer(Duration::from_secs(3)).await;
            Err("Browser input timed out; observe again rather than retrying the mutation.".into())
        })
        .await
    };
    let down_result = wait(down).await;
    let up_result = wait(host.webview.call_cdp(method, &up)).await;
    down_result?;
    up_result?;
    completion.check(guard)
}

/// CDP modifier bits for `Input.dispatchKeyEvent`, mirroring the protocol's
/// `Control+`/`Shift+`/`Alt+` prefixes.
const ALT_MODIFIER: u8 = 1;
const CONTROL_MODIFIER: u8 = 2;
const SHIFT_MODIFIER: u8 = 8;

/// One mapped key press: the CDP `key`/`code` names, the optional text the
/// press types, the Windows virtual key code, and the modifier bits.
struct MappedKey {
    key: String,
    code: String,
    text: Option<String>,
    vk: u32,
    modifiers: u8,
}

/// Map a validated `Press` combination onto CDP key-event fields. The protocol
/// has already validated the grammar; anything unexpected here is a protocol
/// error and is refused rather than guessed at.
fn map_key(combination: &str) -> Result<MappedKey, String> {
    let mut modifiers = 0u8;
    let mut main = combination;
    for _ in 0..3 {
        match main.split_once('+') {
            Some(("Control", tail)) if !tail.is_empty() => {
                modifiers |= CONTROL_MODIFIER;
                main = tail;
            }
            Some(("Shift", tail)) if !tail.is_empty() => {
                modifiers |= SHIFT_MODIFIER;
                main = tail;
            }
            Some(("Alt", tail)) if !tail.is_empty() => {
                modifiers |= ALT_MODIFIER;
                main = tail;
            }
            _ => break,
        }
    }
    let named = match main {
        "Enter" => Some(("Enter", "Enter", Some("\r"), 13)),
        "Tab" => Some(("Tab", "Tab", None, 9)),
        "Escape" => Some(("Escape", "Escape", None, 27)),
        "Backspace" => Some(("Backspace", "Backspace", None, 8)),
        "Delete" => Some(("Delete", "Delete", None, 46)),
        "Insert" => Some(("Insert", "Insert", None, 45)),
        "Home" => Some(("Home", "Home", None, 36)),
        "End" => Some(("End", "End", None, 35)),
        "PageUp" => Some(("PageUp", "PageUp", None, 33)),
        "PageDown" => Some(("PageDown", "PageDown", None, 34)),
        "ArrowUp" => Some(("ArrowUp", "ArrowUp", None, 38)),
        "ArrowDown" => Some(("ArrowDown", "ArrowDown", None, 40)),
        "ArrowLeft" => Some(("ArrowLeft", "ArrowLeft", None, 37)),
        "ArrowRight" => Some(("ArrowRight", "ArrowRight", None, 39)),
        "Space" => Some((" ", "Space", Some(" "), 32)),
        _ => None,
    };
    if let Some((key, code, text, vk)) = named {
        return Ok(MappedKey {
            key: key.to_owned(),
            code: code.to_owned(),
            text: text.map(str::to_owned),
            vk,
            modifiers,
        });
    }
    // Not a named key: the allowlist leaves exactly one printable ASCII char,
    // and a `+` that is the key itself arrives here once a prefix is stripped.
    let mut chars = main.chars();
    let single = match (chars.next(), chars.next()) {
        (Some(ch), None) if ch.is_ascii_graphic() => ch,
        _ => return Err("The key is not in the supported key set.".into()),
    };
    let (vk, code) = match single {
        'a'..='z' | 'A'..='Z' | '0'..='9' => {
            let vk = single.to_ascii_uppercase() as u32;
            let code = if single.is_ascii_alphabetic() {
                format!("Key{}", single.to_ascii_uppercase())
            } else {
                format!("Digit{single}")
            };
            (vk, code)
        }
        ';' => (186, "Semicolon".to_owned()),
        '=' | '+' => (187, "Equal".to_owned()),
        ',' => (188, "Comma".to_owned()),
        '-' => (189, "Minus".to_owned()),
        '.' => (190, "Period".to_owned()),
        '/' => (191, "Slash".to_owned()),
        '`' => (192, "Backquote".to_owned()),
        '[' => (219, "BracketLeft".to_owned()),
        '\\' => (220, "Backslash".to_owned()),
        ']' => (221, "BracketRight".to_owned()),
        '\'' => (222, "Quote".to_owned()),
        _ => return Err("The key is not in the supported key set.".into()),
    };
    Ok(MappedKey {
        key: single.to_string(),
        code,
        text: Some(single.to_string()),
        vk,
        modifiers,
    })
}

/// What one executed action produced: a JSON value, or a bounded media
/// payload that travels as its own wire shape.
pub(super) enum BrowserOutcome {
    Json(Value),
    Media { mime: String, data: String },
}

impl BrowserOutcome {
    fn into_result(self) -> BrowserResult {
        match self {
            Self::Json(value) => BrowserResult::Ok { value },
            Self::Media { mime, data } => {
                BrowserResult::media(&mime, data).unwrap_or_else(|message| {
                    BrowserResult::Error { message }
                })
            }
        }
    }
}

async fn execute(
    host: Rc<WebviewHost>,
    action: BrowserAction,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
) -> Result<BrowserOutcome, String> {
    action.validate()?;
    // A page grant never opens or closes a page; the app host owns both.
    // `Navigate` acknowledges dispatch without waiting for the load: completion
    // is not success, the document guard rotates on the load events. A
    // screenshot observes the visible page directly, so it is answered here
    // too, before the frame and isolated-world setup below.
    match &action {
        BrowserAction::Open { .. } => {
            return Err(
                "Opening a new page is handled by the app host, not by this page's grant.".into(),
            );
        }
        BrowserAction::Close => {
            return Err(
                "Closing a page is handled by the app host, not by this page's grant.".into(),
            );
        }
        BrowserAction::Navigate { url } => {
            valid_url(url)?;
            let url = url.clone();
            let response = cdp_with_completion(
                &host,
                "Page.navigate",
                json!({"url":url}),
                guard,
                executor,
                CompletionGuard::Issued,
            )
            .await?;
            if response
                .get("errorText")
                .and_then(Value::as_str)
                .is_some_and(|error| !error.is_empty())
            {
                return Err("The browser could not navigate to the requested page; observe again without retrying automatically.".into());
            }
            return Ok(BrowserOutcome::Json(json!({"issued":true,"requiresObservation":true})));
        }
        BrowserAction::Screenshot { full_page } => {
            let full_page = *full_page;
            let mut parameters = json!({"format":"png"});
            if full_page {
                parameters["captureBeyondViewport"] = json!(true);
            }
            let data = cdp_capture(&host, parameters, guard, executor).await?;
            // The media budget is stated in decoded bytes; standard base64
            // expands three bytes into four characters.
            let max_encoded = 4 * MAX_BROWSER_MEDIA_BYTES.div_ceil(3);
            if data.len() <= max_encoded {
                return Ok(BrowserOutcome::Media {
                    mime: "image/png".to_owned(),
                    data,
                });
            }
            // One bounded retry as JPEG before refusing: a busy page usually
            // compresses far smaller in a lossy format.
            let mut retry = json!({"format":"jpeg","quality":80});
            if full_page {
                retry["captureBeyondViewport"] = json!(true);
            }
            let data = cdp_capture(&host, retry, guard, executor).await?;
            if data.len() > max_encoded {
                return Err(
                    "The page screenshot exceeds the size limit even as JPEG; reduce the window size."
                        .into(),
                );
            }
            return Ok(BrowserOutcome::Media {
                mime: "image/jpeg".to_owned(),
                data,
            });
        }
        _ => {}
    }
    let tree = cdp(&host, "Page.getFrameTree", json!({}), guard, executor).await?;
    let frame = tree
        .pointer("/frameTree/frame/id")
        .and_then(Value::as_str)
        .ok_or("Main document is unavailable")?;
    let world = cdp(&host, "Page.createIsolatedWorld", json!({"frameId":frame,"worldName":"fintwind-browser-collaboration","grantUniversalAccess":false}), guard, executor).await?;
    let context = world
        .get("executionContextId")
        .and_then(Value::as_i64)
        .ok_or("Isolated document context is unavailable")?;
    // Observation and a scroll act inside the isolated world; the former builds
    // the element map, the latter nudges the real viewport. Everything from
    // here on owns its action: the early arms above only borrowed it.
    match action {
        BrowserAction::Snapshot => {
            evaluate(&host, context, SNAPSHOT.to_owned(), guard, executor)
                .await
                .map(BrowserOutcome::Json)
        }
        BrowserAction::Scroll { delta_y } => {
            let clamped = delta_y.clamp(-MAX_SCROLL_DELTA, MAX_SCROLL_DELTA);
            let expression = format!(
                "(()=>{{try{{window.scrollBy(0,{clamped});}}catch(e){{}}return {{issued:true,requiresObservation:true}};}})()"
            );
            evaluate(&host, context, expression, guard, executor)
                .await
                .map(BrowserOutcome::Json)
        }
        BrowserAction::Evaluate { expression } => {
            evaluate_promise(&host, context, expression, guard, executor)
                .await
                .map(BrowserOutcome::Json)
        }
        BrowserAction::Click { selector } => {
            let (x, y) = resolve_target(&host, context, &selector, false, false, guard, executor).await?;
            input_pair(
                &host,
                "Input.dispatchMouseEvent",
                json!({"type":"mousePressed","x":x,"y":y,"button":"left","clickCount":1}),
                json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}),
                guard,
                executor,
                CompletionGuard::Issued,
            )
            .await?;
            Ok(BrowserOutcome::Json(
                json!({"issued":true,"requiresObservation":true}),
            ))
        }
        BrowserAction::Fill { selector, text } => {
            // A fill resolves with focus so the resolver stores the element as
            // `__fintwindFillTarget`; the checks below read it back.
            resolve_target(&host, context, &selector, true, true, guard, executor).await?;
            evaluate(&host, context, FILL_TARGET_CHECK.into(), guard, executor).await?;
            input_pair(
                &host,
                "Input.dispatchKeyEvent",
                json!({"type":"keyDown","key":"Backspace","code":"Backspace","windowsVirtualKeyCode":8}),
                json!({"type":"keyUp","key":"Backspace","code":"Backspace","windowsVirtualKeyCode":8}),
                guard,
                executor,
                CompletionGuard::SameDocument,
            )
            .await?;
            evaluate(&host, context, FILL_TARGET_CHECK.into(), guard, executor).await?;
            cdp(
                &host,
                "Input.insertText",
                json!({"text":text}),
                guard,
                executor,
            )
            .await?;
            Ok(BrowserOutcome::Json(
                json!({"issued":true,"requiresObservation":true}),
            ))
        }
        BrowserAction::ClickAt { x, y } => {
            // The protocol bounds a coordinate, but only the live viewport
            // knows where the page actually ends; a point beyond it is refused
            // rather than clipped onto a different target.
            let viewport = evaluate(
                &host,
                context,
                "(()=>({w:innerWidth,h:innerHeight}))()".to_owned(),
                guard,
                executor,
            )
            .await?;
            let (Some(w), Some(h)) = (
                viewport.get("w").and_then(Value::as_f64),
                viewport.get("h").and_then(Value::as_f64),
            ) else {
                return Err("The page viewport is unavailable.".into());
            };
            if x as f64 > w || y as f64 > h {
                return Err(format!(
                    "The click coordinates ({x},{y}) are outside the viewport ({w:.0}×{h:.0}); take a snapshot and aim inside it."
                ));
            }
            input_pair(
                &host,
                "Input.dispatchMouseEvent",
                json!({"type":"mousePressed","x":x,"y":y,"button":"left","clickCount":1}),
                json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}),
                guard,
                executor,
                CompletionGuard::Issued,
            )
            .await?;
            Ok(BrowserOutcome::Json(
                json!({"issued":true,"requiresObservation":true}),
            ))
        }
        BrowserAction::DoubleClick { selector } => {
            let (x, y) = resolve_target(&host, context, &selector, false, false, guard, executor).await?;
            input_pair(
                &host,
                "Input.dispatchMouseEvent",
                json!({"type":"mousePressed","x":x,"y":y,"button":"left","clickCount":2}),
                json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":2}),
                guard,
                executor,
                CompletionGuard::Issued,
            )
            .await?;
            Ok(BrowserOutcome::Json(
                json!({"issued":true,"requiresObservation":true}),
            ))
        }
        BrowserAction::Press { selector, key } => {
            // The same focus resolution a fill uses, without its input-only
            // restriction: the key press must land on the resolved control —
            // a button, a card — not on whatever held focus before.
            resolve_target(&host, context, &selector, true, false, guard, executor).await?;
            evaluate(&host, context, FILL_TARGET_CHECK.into(), guard, executor).await?;
            let mapped = map_key(&key)?;
            // `text` is what the press types into the page; with Ctrl or Alt
            // held it would type the wrong thing, so it is only sent for
            // plain and shifted presses.
            let text = match &mapped.text {
                Some(text) if (mapped.modifiers & (CONTROL_MODIFIER | ALT_MODIFIER)) == 0 => {
                    Some(text.as_str())
                }
                _ => None,
            };
            let mut down = json!({
                "type":"keyDown",
                "key":mapped.key,
                "code":mapped.code,
                "windowsVirtualKeyCode":mapped.vk,
                "nativeVirtualKeyCode":mapped.vk,
            });
            if let Some(text) = text {
                down["text"] = json!(text);
            }
            if mapped.modifiers != 0 {
                down["modifiers"] = json!(mapped.modifiers);
            }
            let mut up = json!({
                "type":"keyUp",
                "key":mapped.key,
                "code":mapped.code,
                "windowsVirtualKeyCode":mapped.vk,
                "nativeVirtualKeyCode":mapped.vk,
            });
            if mapped.modifiers != 0 {
                up["modifiers"] = json!(mapped.modifiers);
            }
            input_pair(
                &host,
                "Input.dispatchKeyEvent",
                down,
                up,
                guard,
                executor,
                CompletionGuard::Issued,
            )
            .await?;
            Ok(BrowserOutcome::Json(
                json!({"issued":true,"requiresObservation":true}),
            ))
        }
        BrowserAction::Hover { selector } => {
            let (x, y) = resolve_target(&host, context, &selector, false, false, guard, executor).await?;
            cdp(
                &host,
                "Input.dispatchMouseEvent",
                json!({"type":"mouseMoved","x":x,"y":y}),
                guard,
                executor,
            )
            .await?;
            Ok(BrowserOutcome::Json(
                json!({"issued":true,"requiresObservation":true}),
            ))
        }
        BrowserAction::Select { selector, value } => {
            // The select acts in the DOM, not at coordinates: no input event
            // pair, and the page's own change listeners do the rest.
            let selector = serde_json::to_string(&selector).map_err(|_| "Invalid selector")?;
            let value = serde_json::to_string(&value).map_err(|_| "Invalid select value")?;
            let result = evaluate(
                &host,
                context,
                select_expression(&selector, &value),
                guard,
                executor,
            )
            .await?;
            if result.get("issued").and_then(Value::as_bool) != Some(true) {
                let message = result
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| result.get("reason").and_then(Value::as_str))
                    .unwrap_or(
                        "The target could not be resolved; observe again with a fresh snapshot.",
                    );
                return Err(message.to_owned());
            }
            Ok(BrowserOutcome::Json(
                json!({"issued":true,"requiresObservation":true}),
            ))
        }
        BrowserAction::Drag { from, to } => {
            let start = resolve_target(&host, context, &from, false, false, guard, executor).await?;
            let end = resolve_target(&host, context, &to, false, false, guard, executor).await?;
            dispatch_drag(&host, start, end, guard, executor).await?;
            Ok(BrowserOutcome::Json(
                json!({"issued":true,"requiresObservation":true}),
            ))
        }
        // Refused or answered before the isolated world was created.
        BrowserAction::Open { .. }
        | BrowserAction::Close
        | BrowserAction::Navigate { .. }
        | BrowserAction::Screenshot { .. } => unreachable!(),
    }
}

/// Build the target-resolution expression for a click-like action. `selector`
/// is already a JSON string literal — a `ref:` token or exact CSS — placed
/// directly into the script. With `focus`, the expression focuses the target
/// and records it as `globalThis.__fintwindFillTarget` for the native
/// follow-up checks; with `require_input` additionally, only a fillable
/// input/textarea may be focused and its contents selected — a press, which
/// also focuses, must reach buttons and other controls too.
fn target_expression(selector: &str, focus: bool, require_input: bool) -> String {
    let fill_literal = if focus { "true" } else { "false" };
    let strict_literal = if require_input { "true" } else { "false" };
    format!(
        "(async () => {{\n  const fail=(reason,message)=>({{ok:false,reason,message}});\n  const SEL={selector};\n  const FILL={fill_literal};\n  const STRICT={strict_literal};\n  let el=null;\n  if(SEL.slice(0,4)==='ref:'){{\n    const store=globalThis.__fintwindElementRefs;\n    el=(store&&store.get)?store.get(SEL):undefined;\n    if(!el||!el.isConnected)return fail('stale','target reference is stale; take a fresh snapshot');\n  }} else {{\n    let nodes;\n    try{{nodes=document.querySelectorAll(SEL);}}catch(e){{return fail('invalid','the selector is not valid CSS');}}\n    if(nodes.length===0)return fail('no-matches','no element matches the selector');\n    if(nodes.length>1)return fail('multiple','the selector matches multiple elements; make it unique');\n    el=nodes[0];\n  }}\n  const tag=el.tagName||'';const t=(el.type||'').toLowerCase();\n  if(el.disabled===true||(el.getAttribute&&el.getAttribute('aria-disabled')==='true'))return fail('disabled','the target is disabled');\n  if(t==='password'||t==='file')return fail('password','password and file targets are not supported');\n  if(STRICT){{\n    if(tag!=='INPUT'&&tag!=='TEXTAREA')return fail('unsupported','fill requires an input or textarea');\n    if(el.readOnly===true)return fail('readonly','the target is read-only');\n    if(tag==='INPUT'&&['text','search','email','url','tel'].indexOf(t)<0)return fail('unsupported','this input type cannot be filled');\n  }}\n  const st=getComputedStyle(el);const r0=el.getBoundingClientRect();\n  if(!st||st.visibility!=='visible'||st.display==='none'||(st.opacity!==''&&parseFloat(st.opacity)===0)||r0.width<=0||r0.height<=0)return fail('hidden','the target is not visible');\n  // A page can animate its own scrolling (the site sets `scroll-behavior:smooth`\n  // on html), so a fixed frame count can still measure coordinates mid-flight.\n  // Wait for scrolling to actually stop - bounded, with a timer race so a\n  // throttled animation frame cannot stall the operation. Two quiet readings\n  // keep a mid-animation frame from being mistaken for rest, and the point is\n  // chosen afterwards, so a scroll that starts mid-measure cannot move it.\n  const settle=async function(){{\n    const until=performance.now()+600;let lx=null,ly=null,stable=0;\n    while(performance.now()<until){{\n      await new Promise(function(res){{let done=false;const fin=function(){{if(!done){{done=true;res();}}}};requestAnimationFrame(fin);setTimeout(fin,32);}});\n      const x=window.scrollX||0,y=window.scrollY||0;\n      if(x===lx&&y===ly){{if(++stable>=2)return true;}}else{{stable=0;lx=x;ly=y;}}\n    }}\n    return false;\n  }};\n  const bringIntoView=function(){{\n    try{{el.scrollIntoView({{block:'center',inline:'nearest',behavior:'instant'}});}}\n    catch(e){{try{{el.scrollIntoView({{block:'center',inline:'nearest'}});}}catch(e2){{try{{el.scrollIntoView();}}catch(e3){{}}}}}}\n  }};\n  // Click a real box of the target, not the middle of its bounding rect: an\n  // inline element that wraps spans every line, so the center of the union\n  // rect can fall in the blank space between lines and reach the wrapper\n  // instead of the target, and a control taller than the viewport can never\n  // fit its whole rect on screen. Take the first client box whose center is\n  // on screen and hit-tests to the target or one of its descendants, so a\n  // line hidden under a sticky header is skipped instead of clicked through.\n  // A genuinely covered target still fails: this never falls back to a DOM click.\n  const choosePoint=function(){{\n    let list;\n    try{{list=el.getClientRects?el.getClientRects():[];}}catch(e){{list=[];}}\n    const boxes=(list&&list.length)?Array.from(list):[el.getBoundingClientRect()];\n    let onScreen=0;\n    for(let i=0;i<boxes.length;i++){{\n      const box=boxes[i];const x=box.left+box.width/2,y=box.top+box.height/2;\n      if(x<0||y<0||x>innerWidth||y>innerHeight)continue;\n      onScreen++;\n      let hit=null;\n      try{{hit=document.elementFromPoint(x,y);}}catch(e2){{}}\n      if(hit===el||(el.contains&&el.contains(hit))){{\n        return {{ok:true,onScreen:true,x:Math.min(Math.max(x,0),innerWidth),y:Math.min(Math.max(y,0),innerHeight)}};\n      }}\n    }}\n    return {{ok:false,onScreen:onScreen>0}};\n  }};\n  let chosen=choosePoint();\n  if(!chosen.ok)bringIntoView();\n  if(!(await settle()))return fail('scroll-timeout','scrolling did not settle; take a fresh snapshot once the page stops moving');\n  chosen=choosePoint();\n  if(!chosen.onScreen)return fail('out-of-viewport','the target is outside the viewport after scrolling');\n  if(!chosen.ok)return fail('occluded','another element covers the target');\n  if(FILL){{try{{el.focus();if(STRICT&&el.select)el.select();}}catch(e){{}}globalThis.__fintwindFillTarget=el;}}\n  return {{ok:true,x:chosen.x,y:chosen.y,tag:tag.toLowerCase()}};\n}})()"
    )
}

/// Resolve a target (a `ref:` token through the snapshot map, or exact CSS)
/// and validate it, returning a structured reason instead of a thrown value.
/// A target with no clickable point on screen is scrolled into view and given
/// a bounded settle — the page may animate its own scrolling — before the
/// coordinates are computed from a real client box of the target.
async fn resolve_target(
    host: &WebviewHost,
    context: i64,
    selector: &str,
    focus: bool,
    require_input: bool,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
) -> Result<(f64, f64), String> {
    let selector = serde_json::to_string(selector).map_err(|_| "Invalid selector")?;
    let expression = target_expression(&selector, focus, require_input);
    let target = evaluate_promise(host, context, expression, guard, executor).await?;
    if target.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = target
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| target.get("reason").and_then(Value::as_str))
            .unwrap_or("The target could not be resolved; observe again with a fresh snapshot.");
        return Err(message.to_owned());
    }
    let x = target
        .get("x")
        .and_then(Value::as_f64)
        .ok_or("No target x coordinate")?;
    let y = target
        .get("y")
        .and_then(Value::as_f64)
        .ok_or("No target y coordinate")?;
    Ok((x, y))
}

/// Build the one-shot select expression. It resolves the target exactly like
/// the click resolver — token or unique CSS, enabled, visible — but acts in
/// the DOM instead of at coordinates: it picks the option whose value equals
/// the requested one, sets it, and fires `input`/`change` so page frameworks
/// observe the edit like a real user change. `selector` and `value` are JSON
/// string literals.
fn select_expression(selector: &str, value: &str) -> String {
    format!(
        "(()=>{{\n  const fail=(reason,message)=>({{ok:false,reason,message}});\n  const SEL={selector};\n  const VALUE={value};\n  let el=null;\n  if(SEL.slice(0,4)==='ref:'){{\n    const store=globalThis.__fintwindElementRefs;\n    el=(store&&store.get)?store.get(SEL):undefined;\n    if(!el||!el.isConnected)return fail('stale','target reference is stale; take a fresh snapshot');\n  }} else {{\n    let nodes;\n    try{{nodes=document.querySelectorAll(SEL);}}catch(e){{return fail('invalid','the selector is not valid CSS');}}\n    if(nodes.length===0)return fail('no-matches','no element matches the selector');\n    if(nodes.length>1)return fail('multiple','the selector matches multiple elements; make it unique');\n    el=nodes[0];\n  }}\n  if(el.tagName!=='SELECT')return fail('unsupported','the target is not a select');\n  if(el.disabled===true||(el.getAttribute&&el.getAttribute('aria-disabled')==='true'))return fail('disabled','the target is disabled');\n  const st=getComputedStyle(el);const r0=el.getBoundingClientRect();\n  if(!st||st.visibility!=='visible'||st.display==='none'||(st.opacity!==''&&parseFloat(st.opacity)===0)||r0.width<=0||r0.height<=0)return fail('hidden','the target is not visible');\n  let option=null;\n  for(const o of el.options){{if(o.value===VALUE){{option=o;break;}}}}\n  if(!option)return fail('no-matches','the select has no option with that value');\n  if(option.disabled)return fail('disabled','the option is disabled');\n  if(el.value!==VALUE){{\n    el.value=VALUE;\n    el.dispatchEvent(new Event('input',{{bubbles:true}}));\n    el.dispatchEvent(new Event('change',{{bubbles:true}}));\n  }}\n  return {{issued:true,requiresObservation:true}};\n}})()"
    )
}

/// Drag from one resolved point to another: press, walk eight interpolated
/// moves, release. Once the press is issued the release is mandatory cleanup
/// even when a move step failed — a pressed button must never be left
/// hanging, the same contract `input_pair` enforces for down/up pairs.
async fn dispatch_drag(
    host: &WebviewHost,
    from: (f64, f64),
    to: (f64, f64),
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
) -> Result<(), String> {
    let (fx, fy) = from;
    let (tx, ty) = to;
    cdp_with_completion(
        host,
        "Input.dispatchMouseEvent",
        json!({"type":"mousePressed","x":fx,"y":fy,"button":"left","clickCount":1}),
        guard,
        executor,
        CompletionGuard::Issued,
    )
    .await?;
    let mut moves = Ok(());
    for step in 1..=8 {
        let t = f64::from(step) / 8.0;
        let x = fx + (tx - fx) * t;
        let y = fy + (ty - fy) * t;
        if let Err(error) = cdp(
            host,
            "Input.dispatchMouseEvent",
            json!({"type":"mouseMoved","x":x,"y":y,"button":"left"}),
            guard,
            executor,
        )
        .await
        {
            moves = Err(error);
            break;
        }
    }
    let release = cdp_with_completion(
        host,
        "Input.dispatchMouseEvent",
        json!({"type":"mouseReleased","x":tx,"y":ty,"button":"left","clickCount":1}),
        guard,
        executor,
        CompletionGuard::Issued,
    )
    .await;
    moves?;
    release.map(|_| ())
}

const FILL_TARGET_CHECK: &str = "(() => { const e=globalThis.__fintwindFillTarget; if(!e || !e.isConnected || document.activeElement!==e || e.disabled || e.readOnly || (e.tagName==='INPUT'&&!['text','search','email','url','tel'].includes((e.type||'').toLowerCase()))) throw new Error(); return true; })()";

/// Observe → reference. Runs in the isolated world, mints a per-snapshot nonce,
/// and records each visible control's element object in a fresh
/// `globalThis.__fintwindElementRefs` map keyed by `ref:<nonce>:<ordinal>` — a
/// plain object map, never a DOM property — so an action can turn a token back
/// into the exact element it observed. A new snapshot replaces the map, so any
/// token from an older one stops resolving (an explicit stale error); a document
/// change tears the world down and does the same.
///
/// Sensitive fields are never read: input values are not exported, and a
/// password/hidden/file control is skipped entirely (its name/label is not
/// leaked either). Text below form fields is omitted so a textarea's default
/// value cannot surface. Names come only from aria/associated labels, bounded.
/// This intentionally does not read cookies, storage or raw HTML.
const SNAPSHOT: &str = r#"(() => {
  const clip=(text,limit)=>{text=String(text==null?'':text);return text.slice(0,Math.max(0,limit)).toWellFormed();};
  const visibleText=(root,limit,budget)=>{
   let text='',scanned=0;
   if(!root)return text;
   const walker=document.createTreeWalker(root,NodeFilter.SHOW_TEXT);
   while(scanned++<budget && text.length<limit && walker.nextNode()) {
    const node=walker.currentNode,e=node.parentElement;
    if(!e||e.closest('input,textarea,select,script,style,noscript,template,[hidden]'))continue;
    const s=getComputedStyle(e),r=e.getBoundingClientRect();
    if(s.display==='none'||s.visibility!=='visible'||r.width<=0||r.height<=0)continue;
    const part=node.textContent.trim();
    if(part){const separator=text?'\n':'';text+=separator+clip(part,limit-text.length-separator.length);}
   }
   return clip(text,limit);
  };
  const controlName=(e)=>{
   const t=(e.type||'').toLowerCase();
   if(t==='password'||t==='hidden'||t==='file')return '';
   let al=e.getAttribute&&e.getAttribute('aria-label'); if(al&&al.trim())return clip(al.trim(),120);
   const alby=e.getAttribute&&e.getAttribute('aria-labelledby');
    if(alby){let out='';alby.split(/\s+/).slice(0,16).forEach(function(id){const n=id&&document.getElementById(id); if(n&&out.length<120)out+=' '+visibleText(n,120-out.length,200);}); out=out.trim(); if(out)return clip(out,120);}
    if(e.labels&&e.labels.length){let out='';for(const l of Array.from(e.labels).slice(0,8)){if(out.length>=120)break;out+=' '+visibleText(l,120-out.length,200);} out=out.trim(); if(out)return clip(out,120);}
    if(e.id){let l=null; try{l=document.querySelector('label[for="'+e.id.replace(/["\\]/g,'\\$&')+'"]');}catch(err){} if(l){const x=visibleText(l,120,200); if(x)return clip(x,120);}}
   let wrap=null; try{wrap=e.closest('label');}catch(err){}
    if(wrap){const x=visibleText(wrap,120,200); if(x)return clip(x,120);}
   const ph=e.getAttribute&&e.getAttribute('placeholder'); if(ph&&ph.trim())return clip(ph.trim(),120);
   const vt=visibleText(e,120,200); if(vt)return clip(vt,120);
   return '';
  };
  const nonce=(globalThis.crypto&&crypto.randomUUID)?crypto.randomUUID():('ns'+Date.now().toString(36)+Math.random().toString(36).slice(2));
  const store=(globalThis.__fintwindElementRefs=new Map());
  const controls=[];
  const elements=document.createTreeWalker(document.documentElement,NodeFilter.SHOW_ELEMENT);
  let scannedControls=0,e;
  while(scannedControls++<2000 && controls.length<60 && (e=elements.nextNode())) {
   if(!e.matches('button,a[href],input,textarea,select,[role="button"],[role="link"],[role="textbox"],[role="checkbox"],[role="tab"],[role="menuitem"],[role="option"]'))continue;
   const t=(e.type||'').toLowerCase();
   if(t==='password'||t==='hidden'||t==='file')continue;
   const r=e.getBoundingClientRect(),s=getComputedStyle(e);
   if(r.width<=0||r.height<=0||s.display==='none'||s.visibility!=='visible')continue;
   const ref='ref:'+nonce+':'+controls.length;
   store.set(ref,e);
   controls.push({tag:(e.tagName||'').toLowerCase(),role:clip((e.getAttribute&&e.getAttribute('role'))||(e.tagName||'').toLowerCase(),64),name:controlName(e),ref:ref,selector:ref,disabled:!!(e.disabled===true||(e.getAttribute&&e.getAttribute('aria-disabled')==='true'))});
  }
  const text=visibleText(document.body,8000,2000);
  const result={url:clip(location.href,4096),title:clip(document.title,256),text:text,controls:controls,truncated:true,scope:'main_document',untrustedPageContent:true,refNonce:nonce};
  const encoder=new TextEncoder();
  let sizeGuard=0;
  while(encoder.encode(JSON.stringify(result)).length>28000 && sizeGuard++<40) {
   if(result.text.length)result.text=clip(result.text,Math.floor(result.text.length/2));
   else if(result.controls.length){const dropped=result.controls.pop(); if(dropped&&store.delete)store.delete(dropped.ref);}
   else break;
  }
  return result;
})()"#;
