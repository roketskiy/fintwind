//! Full-access browser launch and lifecycle. No grant is inferred from cwd or
//! the active global window; the GUI publishes a live runtime-scoped launcher.

use super::*;
use gpui::Window;
use serde_json::json;
use std::time::{Duration, Instant};

impl Fintwind {
    pub(in crate::app) fn browser_full_access(&self) -> bool {
        !self.daemon.is_remote()
            && self.selected_session().is_some_and(|session| {
                session.runtime_mode.access() == RuntimeMode::FullAccess
                    && session.interaction_mode == InteractionMode::Build
            })
    }

    pub(in crate::app) fn sync_browser_automation_host(&mut self, cx: &mut Context<Self>) {
        if !self.browser_full_access() && !self.browser_collaboration.automation_pages.is_empty() {
            self.pause_browser_automation(cx);
        }
        let runtime = self.selected_session().and_then(|session| {
            (self.browser_full_access()
                && !self.browser_collaboration.automation_paused
                && self.runtimes.contains_key(&session.id))
            .then_some(session.runtime_event_cursor)
            .flatten()
            .map(|cursor| (session.id, cursor.runtime_id))
            .filter(|runtime| Some(*runtime) != self.browser_collaboration.retired_browser_runtime)
        });
        let stale_pages = self
            .right_panel_browsers
            .iter()
            .filter_map(|(id, browser)| {
                let share = browser.read(cx).browser_share()?;
                let current = self.selected_session().is_some_and(|session| {
                    session.id == share.scope.session_id
                        && self.runtimes.contains_key(&session.id)
                        && session
                            .runtime_event_cursor
                            .is_some_and(|cursor| cursor.runtime_id == share.scope.runtime_id)
                });
                (!current).then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in stale_pages {
            // This is lifecycle retirement, not human takeover. Its delayed
            // ShareChanged must not pause a newly issued runtime launcher.
            self.browser_collaboration.automation_pages.remove(&id);
            self.revoke_browser_grant(id, cx);
        }
        let same = self
            .browser_collaboration
            .host
            .as_ref()
            .is_some_and(|host| runtime == Some((host.session_id, host.runtime_id)));
        if same {
            return;
        }
        if self.browser_collaboration.host.is_some() {
            // Runtime replacement cannot leave native input running under the
            // predecessor, even before the daemon's revocation receipt arrives.
            self.pause_browser_automation(cx);
            self.browser_collaboration.automation_paused = false;
        }
        self.withdraw_browser_automation_host();
        let Some((session_id, runtime_id)) = runtime else {
            return;
        };
        let Some(client) = self
            .browser_collaboration
            .client
            .clone()
            .filter(|client| !client.is_disconnected())
        else {
            return;
        };
        let scope = BrowserScope {
            session_id,
            runtime_id,
            page_id: Uuid::new_v4(),
            grant_id: Uuid::new_v4(),
        };
        match client.publish_browser_host(Some(scope.clone())) {
            Ok(()) => {
                self.browser_collaboration.host = Some(scope);
                self.browser_collaboration.retired_browser_runtime = None;
            }
            Err(error) => self.fail_browser_connection(error.to_string(), cx),
        }
    }

    pub(super) fn withdraw_browser_automation_host(&mut self) {
        if self.browser_collaboration.host.take().is_some()
            && let Some(client) = &self.browser_collaboration.client
        {
            let _ = client.publish_browser_host(None);
        }
    }

    pub(in crate::app) fn pause_browser_automation(&mut self, cx: &mut Context<Self>) {
        self.browser_collaboration.automation_paused = true;
        self.withdraw_browser_automation_host();
        let openings = std::mem::take(&mut self.browser_collaboration.openings);
        for page_id in openings.values() {
            if let Some(browser) = self.right_panel_browsers.get(page_id) {
                browser.update(cx, |view, cx| view.finish_browser_opening(cx));
            }
        }
        for id in openings.keys() {
            if let Some(client) = self.browser_collaboration.pending.remove(id) {
                let _ = client.complete_browser_request(
                    *id,
                    BrowserResult::error(
                        "Browser opening stopped because its authorization was withdrawn.",
                    ),
                );
            }
        }
        let pages = std::mem::take(&mut self.browser_collaboration.automation_pages);
        self.browser_collaboration
            .opened_pages
            .retain(|_, scope| !pages.contains(&scope.page_id));
        for id in pages {
            if let Some(browser) = self.right_panel_browsers.get(&id).cloned() {
                browser.update(cx, |view, cx| view.revoke_browser_share(cx));
            }
        }
    }

    pub(in crate::app) fn reset_browser_automation_for_session(&mut self, cx: &mut Context<Self>) {
        self.browser_collaboration.automation_paused = false;
        self.sync_browser_automation_host(cx);
    }

    pub(in crate::app) fn browser_access_mode_changed(&mut self, cx: &mut Context<Self>) {
        // Only explicitly shared pages can follow FullAccess. Manual tabs that
        // were never shared stay invisible to the agent when access changes.
        self.pause_browser_automation(cx);
        self.browser_collaboration.automation_paused = false;
        if self.browser_full_access() {
            let current = self.selected_session().and_then(|session| {
                session
                    .runtime_event_cursor
                    .map(|cursor| (session.id, cursor.runtime_id))
            });
            let shared = self
                .right_panel_browsers
                .iter()
                .filter_map(|(id, browser)| {
                    browser
                        .read(cx)
                        .browser_share()
                        .filter(|share| {
                            current == Some((share.scope.session_id, share.scope.runtime_id))
                        })
                        .map(|_| (*id, browser.clone()))
                })
                .collect::<Vec<_>>();
            for (id, browser) in shared {
                browser.update(cx, |view, cx| view.set_browser_automatic(true, cx));
                self.browser_collaboration.automation_pages.insert(id);
            }
        }
        self.sync_browser_automation_host(cx);
    }

    pub(super) fn open_automation_browser(
        &mut self,
        request: BrowserRequest,
        url: String,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.browser_collaboration.client.clone() else {
            return;
        };
        let allowed = self.browser_full_access()
            && !self.browser_collaboration.automation_paused
            && self.browser_collaboration.host.as_ref() == Some(&request.scope)
            && self.runtimes.contains_key(&request.scope.session_id)
            && self.selected_session().is_some_and(|session| {
                session.id == request.scope.session_id
                    && session
                        .runtime_event_cursor
                        .is_some_and(|cursor| cursor.runtime_id == request.scope.runtime_id)
            });
        let error = if !allowed {
            Some("Browser opening is not authorized for this live full-access session.".to_owned())
        } else if self.browser_collaboration.openings.len() >= 1 {
            Some("Another browser opening is in progress; observe its result first.".to_owned())
        } else if self
            .right_panel_browsers
            .values()
            .filter(|browser| browser.read(cx).browser_share().is_some())
            .count()
            >= MAX_BROWSER_PAGES_PER_CONNECTION
        {
            Some(
                "Too many browser pages are shared; revoke a test page before opening another."
                    .to_owned(),
            )
        } else {
            BrowserAction::validate_url(&url).err()
        };
        if let Some(error) = error {
            let _ =
                client.complete_browser_request(request.request_id, BrowserResult::error(error));
            return;
        }
        let page_id = Uuid::new_v4();
        self.open_right_panel_surface(RightPanelSurface::Browser(page_id), cx);
        // The ordinary toolbar uses native focus for human browsing. An agent
        // launch must display the real page without simulating human takeover.
        self.right_panel_pending_browser_focus = None;
        let browser = self.ensure_right_panel_browser(page_id, window, cx);
        browser.update(cx, |view, cx| view.navigate_for_automation(url, cx));
        self.browser_collaboration
            .pending
            .insert(request.request_id, client);
        self.browser_collaboration
            .openings
            .insert(request.request_id, page_id);
        let scope = BrowserScope {
            session_id: request.scope.session_id,
            runtime_id: request.scope.runtime_id,
            page_id,
            grant_id: Uuid::new_v4(),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(75)).await;
                let done = this.update_in(cx, |this, _, cx| {
                    if this.browser_collaboration.openings.get(&request.request_id) != Some(&page_id) {
                        return true;
                    }
                    if browser.read(cx).browser_automation_was_taken_over() {
                        this.pause_browser_automation(cx);
                        return true;
                    }
                    let current = this.browser_full_access()
                        && this.browser_collaboration.generation == generation
                        && this.browser_collaboration.host.as_ref() == Some(&request.scope)
                        && this.runtimes.contains_key(&request.scope.session_id)
                        && this.selected_session().is_some_and(|session| {
                            session.id == request.scope.session_id
                                && session.runtime_event_cursor.is_some_and(|cursor| {
                                    cursor.runtime_id == request.scope.runtime_id
                                })
                        })
                        && this.right_panel_browsers.contains_key(&page_id)
                        && this.browser_collaboration.client.as_ref().is_some_and(|client| !client.is_disconnected());
                    let ready = browser.read(cx).browser_ready_for_automation();
                    let error = if !current {
                        Some("Browser opening was cancelled or its authorization changed.".to_owned())
                    } else if let Err(error) = &ready {
                        Some(error.clone())
                    } else if Instant::now() >= deadline {
                        Some("The native browser did not finish loading within 15 seconds. No page was shared.".to_owned())
                    } else { None };
                    if let Some(error) = error {
                        this.complete_browser_opening(request.request_id, BrowserResult::error(error), cx);
                        return true;
                    }
                    if ready != Ok(true) { return false }
                    // The publish is fire-and-forget: a daemon refusal
                    // arrives as a receipt only after this tick. Answering in
                    // the same tick would hand out a grant the daemon may be
                    // about to reject, so the publish records the scope here
                    // and the pass below answers only once the share survived
                    // a full tick.
                    if let Some(expected) = this.browser_collaboration.opened_pages.get(&request.request_id).cloned() {
                        let Some(share) = browser.read(cx).browser_share() else {
                            this.complete_browser_opening(request.request_id, BrowserResult::error("The new page could not be published on the live browser connection."), cx);
                            return true;
                        };
                        if share.scope != expected {
                            this.complete_browser_opening(request.request_id, BrowserResult::error("The native page authorization was withdrawn."), cx);
                            return true;
                        }
                        this.complete_browser_opening(request.request_id, BrowserResult::Ok { value: json!({
                            "pageId": share.scope.page_id, "grantId":share.scope.grant_id,
                            "url":share.url, "title":share.title,
                        }) }, cx);
                        return true;
                    }
                    let shared = browser.update(cx, |view, cx| view.begin_browser_automation(scope.clone(), cx));
                    if !shared {
                        this.complete_browser_opening(request.request_id, BrowserResult::error("The native page could not be shared after loading."), cx);
                        return true;
                    }
                    this.browser_collaboration.automation_pages.insert(page_id);
                    if browser.read(cx).browser_share().is_none() {
                        this.complete_browser_opening(request.request_id, BrowserResult::error("The native page authorization was withdrawn."), cx);
                        return true;
                    }
                    this.publish_browser_shares(cx);
                    if browser.read(cx).browser_share().is_none()
                        || this.browser_collaboration.client.is_none()
                    {
                        this.complete_browser_opening(request.request_id, BrowserResult::error("The new page could not be published on the live browser connection."), cx);
                        return true;
                    }
                    this.browser_collaboration.opened_pages.insert(request.request_id, scope.clone());
                    false
                }).unwrap_or(true);
                if done { break }
            }
        }).detach();
    }

    /// Close the page this session's automation opened. Launcher-side like
    /// [`open_automation_browser`]: a page-level grant must never close its
    /// own surface, so the app host answers `Close` here and the view layer
    /// refuses it. Ordering invariants: the request is answered before
    /// anything is torn down, because the teardown retires the view and
    /// republishes the share set, failing anything still pending on the
    /// page; and the page leaves `automation_pages` and `opened_pages`
    /// before that teardown, so the close path's automation retirement does
    /// not read the page's disappearance as a human takeover and pause the
    /// whole automation host — an agent closing its own page is not a
    /// takeover.
    pub(super) fn close_automation_browser(
        &mut self,
        request: BrowserRequest,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.browser_collaboration.client.clone() else {
            // No live connection to answer on; the daemon-side caller times
            // out on its own, which is the honest outcome here.
            return;
        };
        let page_id = request.scope.page_id;
        if !self.browser_full_access() || self.browser_collaboration.automation_paused {
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error(
                    "Closing a page needs this session's full-access browser permission.",
                ),
            );
            return;
        }
        if !self.browser_collaboration.automation_pages.contains(&page_id) {
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error(
                    "Only a page this session opened automatically can be closed by it; a manually shared tab is the user's.",
                ),
            );
            return;
        }
        let surface_index = self
            .right_panel_surfaces
            .iter()
            .position(|surface| matches!(surface, RightPanelSurface::Browser(id) if *id == page_id));
        if surface_index.is_none() && !self.right_panel_browsers.contains_key(&page_id) {
            // Nothing is left to announce the change, so any lingering grant
            // is revoked here by publishing the cache minus this page.
            self.revoke_browser_grant(page_id, cx);
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error("The browser page is already closed."),
            );
            return;
        }
        let granted = self
            .right_panel_browsers
            .get(&page_id)
            .is_some_and(|browser| {
                browser
                    .read(cx)
                    .browser_share()
                    .is_some_and(|share| share.scope == request.scope)
            });
        if !granted {
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error("Page is not shared with this session and runtime."),
            );
            return;
        }
        let Some(surface_index) = surface_index else {
            // Entities and surfaces are removed in lockstep, so this is only
            // reachable if that bookkeeping went wrong; degrade to the
            // already-closed outcome rather than closing a wrong tab.
            self.revoke_browser_grant(page_id, cx);
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error("The browser page is already closed."),
            );
            return;
        };
        // Answer first: the teardown below retires the view and republishes
        // the share set, so the completion must not depend on a state that is
        // mid-removal.
        let _ = client.complete_browser_request(
            request.request_id,
            BrowserResult::Ok {
                value: json!({ "closed": true }),
            },
        );
        // Retire the automation bookkeeping before the close: the close
        // path's automation retirement must not read this page as a human
        // takeover and pause the whole automation host.
        self.browser_collaboration.automation_pages.remove(&page_id);
        self.browser_collaboration
            .opened_pages
            .retain(|_, scope| scope.page_id != page_id);
        self.close_browser_surface(surface_index, page_id, cx);
    }

    /// Close the browser surface at `index`, mirroring the browser branch of
    /// `close_right_panel_surface` in `right_panel.rs`. That method is
    /// private to its module, so its bookkeeping is repeated here instead of
    /// called; keep the two in lockstep. The entity is removed before
    /// `forget_browser_collaboration`, exactly as there, so the republication
    /// excludes this page.
    fn close_browser_surface(&mut self, index: usize, browser_id: Uuid, cx: &mut Context<Self>) {
        self.right_panel_browsers.remove(&browser_id);
        self.forget_browser_collaboration(browser_id, cx);
        self.right_panel_surfaces.remove(index);
        self.right_panel_active_surface = if self.right_panel_surfaces.is_empty() {
            None
        } else {
            Some(match self.right_panel_active_surface {
                Some(active) if active > index => active - 1,
                Some(active) if active == index => index.saturating_sub(1),
                Some(active) => active.min(self.right_panel_surfaces.len() - 1),
                None => 0,
            })
        };
        if let Some(active) = self.right_panel_active_surface {
            // `reveal_right_panel_tab` is likewise private to right_panel.rs;
            // this is its body.
            self.right_panel_pending_tab_reveal = Some(active);
            self.right_panel_tabs_scroll_handle.scroll_to_item(active);
            self.request_active_terminal_focus();
            self.request_active_browser_focus();
        } else {
            self.right_panel_pending_tab_reveal = None;
            self.right_panel_pending_terminal_focus = None;
            self.right_panel_pending_browser_focus = None;
            self.set_right_panel_visible(false, cx);
        }
        cx.notify();
    }

    fn complete_browser_opening(
        &mut self,
        id: Uuid,
        result: BrowserResult,
        cx: &mut Context<Self>,
    ) {
        if let Some(page_id) = self.browser_collaboration.openings.get(&id)
            && let Some(browser) = self.right_panel_browsers.get(page_id)
        {
            browser.update(cx, |view, cx| view.finish_browser_opening(cx));
        }
        self.finish_browser_opening(id, result);
    }

    pub(super) fn finish_browser_opening(&mut self, id: Uuid, result: BrowserResult) {
        self.browser_collaboration.openings.remove(&id);
        if let Some(client) = self.browser_collaboration.pending.remove(&id) {
            let _ = client.complete_browser_request(id, result);
        }
    }
}
