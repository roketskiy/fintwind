//! Session-scoped access to the native browser. Pages are published for the
//! selected live local session, regardless of who opened them or its mode.

use super::*;
use gpui::Window;
use serde_json::json;
use std::time::{Duration, Instant};

impl Fintwind {
    pub(in crate::app) fn sync_browser_automation_host(&mut self, cx: &mut Context<Self>) {
        let runtime = self.selected_session().and_then(|session| {
            (!self.daemon.is_remote() && self.runtimes.contains_key(&session.id))
                .then_some(session.runtime_event_cursor)
                .flatten()
                .map(|cursor| (session.id, cursor.runtime_id))
                .filter(|runtime| {
                    Some(*runtime) != self.browser_collaboration.retired_browser_runtime
                })
        });
        let stale_pages = self
            .right_panel_browsers
            .iter()
            .filter_map(|(id, browser)| {
                let share = browser.read(cx).browser_share()?;
                (runtime != Some((share.scope.session_id, share.scope.runtime_id))).then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in stale_pages {
            self.browser_collaboration.automation_pages.remove(&id);
            self.revoke_browser_grant(id, cx);
        }
        let same = self
            .browser_collaboration
            .host
            .as_ref()
            .is_some_and(|host| runtime == Some((host.session_id, host.runtime_id)));
        if !same {
            if self.browser_collaboration.host.is_some() {
                self.pause_browser_automation(cx);
            }
            self.withdraw_browser_automation_host();
            if let Some((session_id, runtime_id)) = runtime
                && let Some(client) = self
                    .browser_collaboration
                    .client
                    .clone()
                    .filter(|client| !client.is_disconnected())
            {
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
        }
        self.sync_browser_automatic_pages(cx);
    }

    /// Publish only tabs in the selected session's panel. The browser entity
    /// map also contains tabs belonging to other, inactive sessions.
    pub(super) fn sync_browser_automatic_pages(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.browser_collaboration.host.clone() else {
            return;
        };
        let ids = self
            .right_panel_surfaces
            .iter()
            .filter_map(|surface| match surface {
                RightPanelSurface::Browser(id) => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in ids {
            if self
                .browser_collaboration
                .openings
                .values()
                .any(|page| *page == id)
            {
                continue;
            }
            let Some(browser) = self.right_panel_browsers.get(&id).cloned() else {
                continue;
            };
            if browser.read(cx).browser_share().is_some()
                || browser.read(cx).browser_ready_for_automation() != Ok(true)
            {
                continue;
            }
            if self.browser_collaboration.automation_pages.len() >= MAX_BROWSER_PAGES_PER_CONNECTION
            {
                break;
            }
            let scope = BrowserScope {
                session_id: host.session_id,
                runtime_id: host.runtime_id,
                page_id: id,
                grant_id: Uuid::new_v4(),
            };
            if browser.update(cx, |view, cx| view.begin_browser_automation(scope, cx)) {
                self.browser_collaboration.automation_pages.insert(id);
            }
        }
        self.publish_browser_shares(cx);
    }

    pub(super) fn withdraw_browser_automation_host(&mut self) {
        if self.browser_collaboration.host.take().is_some()
            && let Some(client) = &self.browser_collaboration.client
        {
            let _ = client.publish_browser_host(None);
        }
    }

    /// Retire in-flight work and page scopes on connection or runtime changes.
    pub(in crate::app) fn pause_browser_automation(&mut self, cx: &mut Context<Self>) {
        let openings = std::mem::take(&mut self.browser_collaboration.openings);
        for (request_id, page_id) in openings {
            if let Some(browser) = self.right_panel_browsers.get(&page_id) {
                browser.update(cx, |view, cx| view.cancel_browser_opening(cx));
            }
            if let Some(client) = self.browser_collaboration.pending.remove(&request_id) {
                let _ = client.complete_browser_request(
                    request_id,
                    BrowserResult::error("Browser opening stopped because the session changed."),
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
        self.sync_browser_automation_host(cx);
    }

    pub(in crate::app) fn browser_access_mode_changed(&mut self, cx: &mut Context<Self>) {
        // Browser access no longer depends on the session's interaction or
        // approval mode; keep its current scopes and refresh availability.
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
        let error = if !self.local_browser_runtime(&request.scope, generation) {
            Some("The browser session or runtime is no longer active.".to_owned())
        } else if !self.browser_collaboration.openings.is_empty() {
            Some("Another browser opening is in progress; observe its result first.".to_owned())
        } else if self.browser_collaboration.automation_pages.len()
            >= MAX_BROWSER_PAGES_PER_CONNECTION
        {
            Some("Too many browser pages are open for this session.".to_owned())
        } else {
            BrowserAction::validate_url(&url).err()
        };
        if let Some(error) = error {
            let _ =
                client.complete_browser_request(request.request_id, BrowserResult::error(error));
            return;
        }
        self.browser_collaboration
            .pending
            .insert(request.request_id, client);
        self.start_automation_browser_open(request, url, generation, window, cx);
    }

    fn local_browser_runtime(&self, scope: &BrowserScope, generation: u64) -> bool {
        !self.daemon.is_remote()
            && self.browser_collaboration.generation == generation
            && self.browser_collaboration.host.as_ref() == Some(scope)
            && self.runtimes.contains_key(&scope.session_id)
            && self.selected_session().is_some_and(|session| {
                session.id == scope.session_id
                    && session
                        .runtime_event_cursor
                        .is_some_and(|cursor| cursor.runtime_id == scope.runtime_id)
            })
            && self
                .browser_collaboration
                .client
                .as_ref()
                .is_some_and(|client| !client.is_disconnected())
    }

    fn start_automation_browser_open(
        &mut self,
        request: BrowserRequest,
        url: String,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.local_browser_runtime(&request.scope, generation) {
            self.finish_browser_opening(
                request.request_id,
                BrowserResult::error("The browser session changed."),
            );
            return;
        }
        let page_id = Uuid::new_v4();
        self.open_right_panel_surface(RightPanelSurface::Browser(page_id), cx);
        self.right_panel_pending_browser_focus = None;
        let browser = self.ensure_right_panel_browser(page_id, window, cx);
        browser.update(cx, |view, cx| view.navigate_for_automation(url, cx));
        let request_id = request.request_id;
        self.browser_collaboration
            .openings
            .insert(request_id, page_id);
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
                    if this.browser_collaboration.openings.get(&request_id) != Some(&page_id) {
                        return true;
                    }
                    let ready = browser.read(cx).browser_ready_for_automation();
                    let error = if !this.local_browser_runtime(&request.scope, generation) {
                        Some("Browser opening was cancelled because the session changed.".to_owned())
                    } else if let Err(error) = &ready {
                        Some(error.clone())
                    } else if Instant::now() >= deadline {
                        Some("The native browser did not finish loading within 15 seconds.".to_owned())
                    } else { None };
                    if let Some(error) = error {
                        this.complete_browser_opening(request_id, BrowserResult::error(error), cx);
                        return true;
                    }
                    if ready != Ok(true) { return false; }
                    if let Some(expected) = this.browser_collaboration.opened_pages.get(&request_id).cloned() {
                        let Some(share) = browser.read(cx).browser_share() else {
                            this.complete_browser_opening(request_id, BrowserResult::error("The new page could not be published."), cx);
                            return true;
                        };
                        if share.scope != expected {
                            this.complete_browser_opening(request_id, BrowserResult::error("The browser page changed while opening."), cx);
                            return true;
                        }
                        this.complete_browser_opening(request_id, BrowserResult::Ok { value: json!({
                            "pageId": share.scope.page_id, "grantId": share.scope.grant_id,
                            "url": share.url, "title": share.title,
                        }) }, cx);
                        return true;
                    }
                    if !browser.update(cx, |view, cx| view.begin_browser_automation(scope.clone(), cx)) {
                        this.complete_browser_opening(request_id, BrowserResult::error("The native page could not be made available."), cx);
                        return true;
                    }
                    this.browser_collaboration.automation_pages.insert(page_id);
                    this.publish_browser_shares(cx);
                    if browser.read(cx).browser_share().is_none() || this.browser_collaboration.client.is_none() {
                        this.complete_browser_opening(request_id, BrowserResult::error("The new page could not be published."), cx);
                        return true;
                    }
                    this.browser_collaboration.opened_pages.insert(request_id, scope.clone());
                    false
                }).unwrap_or(true);
                if done { break; }
            }
        }).detach();
    }

    /// Any page in the selected session may be closed, irrespective of who
    /// opened it. Answer before removing the surface and publishing its removal.
    pub(super) fn close_automation_browser(
        &mut self,
        request: BrowserRequest,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.browser_collaboration.client.clone() else {
            return;
        };
        let page_id = request.scope.page_id;
        let index = self.right_panel_surfaces.iter().position(
            |surface| matches!(surface, RightPanelSurface::Browser(id) if *id == page_id),
        );
        let granted = self
            .right_panel_browsers
            .get(&page_id)
            .is_some_and(|browser| {
                browser
                    .read(cx)
                    .browser_share()
                    .is_some_and(|share| share.scope == request.scope)
            });
        let current = self.state.selected_session == Some(request.scope.session_id)
            && self.runtimes.contains_key(&request.scope.session_id)
            && self
                .selected_session()
                .and_then(|session| session.runtime_event_cursor)
                .is_some_and(|cursor| cursor.runtime_id == request.scope.runtime_id);
        let Some(index) = index.filter(|_| granted && current) else {
            let _ = client.complete_browser_request(
                request.request_id,
                BrowserResult::error("The browser page is no longer available in this session."),
            );
            return;
        };
        let _ = client.complete_browser_request(
            request.request_id,
            BrowserResult::Ok {
                value: json!({ "closed": true }),
            },
        );
        self.browser_collaboration.automation_pages.remove(&page_id);
        self.browser_collaboration
            .opened_pages
            .retain(|_, scope| scope.page_id != page_id);
        self.close_browser_surface(index, page_id, cx);
    }

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
        let failed = matches!(&result, BrowserResult::Error { .. });
        if let Some(page_id) = self.browser_collaboration.openings.get(&id).copied()
            && let Some(browser) = self.right_panel_browsers.get(&page_id).cloned()
        {
            if failed {
                self.browser_collaboration.automation_pages.remove(&page_id);
                self.browser_collaboration.opened_pages.remove(&id);
                browser.update(cx, |view, cx| {
                    view.cancel_browser_opening(cx);
                    view.revoke_browser_share(cx);
                });
            } else {
                browser.update(cx, |view, cx| view.finish_browser_opening(cx));
            }
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
