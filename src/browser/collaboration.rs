//! Explicit page capabilities and one-at-a-time, user-approved operations.
//!
//! Failure modes: stale grants, navigation during approval/execution, duplicate
//! requests, cancellation between CDP calls, ambiguous/hidden targets, unbounded
//! page data, and accidentally exposing arbitrary JavaScript or raw CDP.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use futures_lite::FutureExt;
use gpui::{Context, EventEmitter, FocusHandle, IntoElement, Window, div, prelude::*, px};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{BrowserView, host::WebviewHost};
use crate::ui::ActivationExt;
use fintwind_protocol::browser::{
    BrowserAction, BrowserRequest, BrowserResult, BrowserScope, BrowserShare,
};

pub(crate) enum BrowserCollaborationEvent {
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

impl BrowserView {
    pub(crate) fn browser_share(&self) -> Option<BrowserShare> {
        self.collaboration.share.clone()
    }

    #[cfg(feature = "browser-poc")]
    pub(crate) fn pending_browser_request(&self) -> Option<BrowserRequest> {
        self.collaboration.pending.clone()
    }

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
        // Sharing is an explicit human action. Start with native typing owned
        // by GPUI so later entry into the page is an observable takeover edge;
        // browser requests themselves never MoveFocus or reclaim focus.
        self.reclaim_native_keyboard(cx);
        cx.emit(BrowserCollaborationEvent::ShareChanged);
        cx.notify();
    }

    pub(crate) fn revoke_browser_share(&mut self, cx: &mut Context<Self>) {
        let changed = self.collaboration.share.take().is_some();
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

    pub(crate) fn handle_browser_request(
        &mut self,
        request: BrowserRequest,
        cx: &mut Context<Self>,
    ) {
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
        if request.action.requires_approval() {
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
        let cancelled = Arc::new(AtomicBool::new(false));
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
                            Ok(value) => BrowserResult::Ok { value },
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

    pub(super) fn render_collaboration_bar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        self.collaboration.share.as_ref()?;
        let theme = crate::theme::Theme::current(cx);
        let controls = self
            .collaboration
            .controls
            .get_or_insert_with(|| {
                [
                    cx.focus_handle(),
                    cx.focus_handle(),
                    cx.focus_handle(),
                    cx.focus_handle(),
                ]
            })
            .clone();
        let pending = self.collaboration.pending.clone();
        let summary = pending
            .as_ref()
            .map(|request| match &request.action {
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
                BrowserAction::Snapshot => tr!("browser_collaboration.reading"),
            })
            .unwrap_or_else(|| {
                if self.collaboration.running.is_some() {
                    tr!("browser_collaboration.running")
                } else {
                    tr!("browser_collaboration.shared_status")
                }
            });
        Some(
            div()
                .id("browser-collaboration-bar")
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .p(px(6.0))
                .bg(theme.surface)
                .text_color(theme.text)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape"
                        && !event.keystroke.modifiers.modified()
                        && let Some(request) = this.collaboration.pending.as_ref()
                    {
                        this.reject_browser_request(request.request_id, cx);
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
                        .max_h(px(100.0))
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
                        .text_size(px(11.0))
                        .text_color(theme.text)
                        .child(summary),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.0))
                        .when_some(pending, |row, pending| {
                            let id = pending.request_id;
                            row.child(
                                div()
                                    .id("browser-approve")
                                    .track_focus(&controls[0])
                                    .tab_index(0)
                                    .min_h(px(32.0))
                                    .px(px(8.0))
                                    .py(px(4.0))
                                    .border_1()
                                    .border_color(theme.border)
                                    .focus_visible(|style| style.border_color(theme.accent))
                                    .cursor_pointer()
                                    .child(tr!("browser_collaboration.approve_once"))
                                    .on_activation(cx, move |this, _, cx| {
                                        this.approve_browser_request(id, cx)
                                    }),
                            )
                            .child(
                                div()
                                    .id("browser-reject")
                                    .track_focus(&controls[1])
                                    .tab_index(0)
                                    .min_h(px(32.0))
                                    .px(px(8.0))
                                    .py(px(4.0))
                                    .border_1()
                                    .border_color(theme.border)
                                    .focus_visible(|style| style.border_color(theme.accent))
                                    .cursor_pointer()
                                    .child(tr!("browser_collaboration.reject"))
                                    .on_activation(cx, move |this, _, cx| {
                                        this.reject_browser_request(id, cx)
                                    }),
                            )
                        })
                        .child(
                            div()
                                .id("browser-revoke")
                                .track_focus(&controls[2])
                                .tab_index(0)
                                .min_h(px(32.0))
                                .px(px(8.0))
                                .py(px(4.0))
                                .border_1()
                                .border_color(theme.border)
                                .focus_visible(|style| style.border_color(theme.accent))
                                .cursor_pointer()
                                .child(tr!("browser_collaboration.take_over"))
                                .on_activation(cx, |this, _, cx| this.revoke_browser_share(cx)),
                        ),
                ),
        )
    }
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
    guard.check()?;
    Ok(value)
}

async fn evaluate(
    host: &WebviewHost,
    context: i64,
    expression: String,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
) -> Result<Value, String> {
    let value = cdp(
        host,
        "Runtime.evaluate",
        json!({"contextId":context,"expression":expression,"returnByValue":true}),
        guard,
        executor,
    )
    .await?;
    if value.get("exceptionDetails").is_some() {
        return Err(
            "The requested element is missing, ambiguous, hidden, disabled, or unsupported.".into(),
        );
    }
    value
        .pointer("/result/value")
        .cloned()
        .ok_or_else(|| "Native page did not return a structured result".into())
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
    guard.check()
}

async fn execute(
    host: Rc<WebviewHost>,
    action: BrowserAction,
    guard: &OperationGuard,
    executor: &gpui::BackgroundExecutor,
) -> Result<Value, String> {
    action.validate()?;
    if let BrowserAction::Navigate { url } = action {
        valid_url(&url)?;
        // Completion acknowledges dispatch, not successful loading. Navigation
        // revokes the grant; the user must share the new document explicitly.
        return cdp(&host, "Page.navigate", json!({"url":url}), guard, executor).await;
    }
    let tree = cdp(&host, "Page.getFrameTree", json!({}), guard, executor).await?;
    let frame = tree
        .pointer("/frameTree/frame/id")
        .and_then(Value::as_str)
        .ok_or("Main document is unavailable")?;
    let world = cdp(&host, "Page.createIsolatedWorld", json!({"frameId":frame,"worldName":"fintwind-browser-collaboration","grantUniveralAccess":false}), guard, executor).await?;
    let context = world
        .get("executionContextId")
        .and_then(Value::as_i64)
        .ok_or("Isolated document context is unavailable")?;
    if matches!(action, BrowserAction::Snapshot) {
        return evaluate(&host, context, SNAPSHOT.to_owned(), guard, executor).await;
    }
    let (selector, fill) = match action {
        BrowserAction::Click { selector } => (selector, None),
        BrowserAction::Fill { selector, text } => (selector, Some(text)),
        _ => unreachable!(),
    };
    let selector = serde_json::to_string(&selector).map_err(|_| "Invalid selector")?;
    let expression = format!(
        "(() => {{ const nodes=document.querySelectorAll({selector}); if(nodes.length!==1) throw new Error(); const e=nodes[0]; const type=(e.type||'').toLowerCase(); if(e.disabled || ['password','hidden','file'].includes(type)) throw new Error(); const r=e.getBoundingClientRect(),s=getComputedStyle(e); if(r.width<=0||r.height<=0||s.visibility!=='visible'||s.display==='none'||r.x<0||r.y<0||r.right>innerWidth||r.bottom>innerHeight) throw new Error(); const x=r.x+r.width/2,y=r.y+r.height/2,hit=document.elementFromPoint(x,y); if(hit!==e&&!e.contains(hit)) throw new Error(); {} return {{x,y}}; }})()",
        if fill.is_some() {
            "if(!['INPUT','TEXTAREA'].includes(e.tagName)||e.readOnly|| (e.tagName==='INPUT'&&!['text','search','email','url','tel'].includes(type)))throw new Error(); e.focus(); e.select(); globalThis.__fintwindFillTarget=e;"
        } else {
            ""
        }
    );
    let target = evaluate(&host, context, expression, guard, executor).await?;
    if let Some(text) = fill {
        evaluate(&host, context, FILL_TARGET_CHECK.into(), guard, executor).await?;
        input_pair(
            &host,
            "Input.dispatchKeyEvent",
            json!({"type":"keyDown","key":"Backspace","code":"Backspace","windowsVirtualKeyCode":8}),
            json!({"type":"keyUp","key":"Backspace","code":"Backspace","windowsVirtualKeyCode":8}),
            guard,
            executor,
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
    } else {
        let x = target
            .get("x")
            .and_then(Value::as_f64)
            .ok_or("No target x coordinate")?;
        let y = target
            .get("y")
            .and_then(Value::as_f64)
            .ok_or("No target y coordinate")?;
        input_pair(
            &host,
            "Input.dispatchMouseEvent",
            json!({"type":"mousePressed","x":x,"y":y,"button":"left","clickCount":1}),
            json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}),
            guard,
            executor,
        )
        .await?;
    }
    Ok(json!({"issued":true,"requiresObservation":true}))
}

const FILL_TARGET_CHECK: &str = "(() => { const e=globalThis.__fintwindFillTarget; if(!e || !e.isConnected || document.activeElement!==e || e.disabled || e.readOnly || (e.tagName==='INPUT'&&!['text','search','email','url','tel'].includes((e.type||'').toLowerCase()))) throw new Error(); return true; })()";

/// Visible, bounded text and controls only. Text nodes below form fields are
/// omitted too: body.innerText can otherwise expose a textarea's default value.
/// This intentionally does not read cookies, storage or raw HTML.
/// Repair only the bounded slice: an emoji cut in half must not send an
/// unpaired UTF-16 surrogate into WebView2's native CDP result path.
const SNAPSHOT: &str = r#"(() => {
 const clip=(text,limit)=>text.slice(0,Math.max(0,limit)).toWellFormed();
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
 const controls=[];
 const elements=document.createTreeWalker(document.documentElement,NodeFilter.SHOW_ELEMENT);
 let scannedControls=0,e;
 while(scannedControls++<2000 && controls.length<60 && (e=elements.nextNode())) {
  if(!e.matches('button,a[href],input,textarea,select,[role="button"]'))continue;
  const t=(e.type||'').toLowerCase(),r=e.getBoundingClientRect(),s=getComputedStyle(e);
  if(['password','hidden','file'].includes(t)||r.width<=0||r.height<=0||s.display==='none'||s.visibility!=='visible')continue;
   controls.push({tag:e.tagName.toLowerCase(),role:clip(e.getAttribute('role')||'',64),name:clip(e.getAttribute('aria-label')||e.getAttribute('placeholder')||visibleText(e,120,200),120),selector:e.id&&e.id.length<=128&&e.id.isWellFormed()?'#'+CSS.escape(e.id):null,disabled:!!e.disabled});
 }
 const text=visibleText(document.body,8000,2000);
 const result={url:clip(location.href,4096),title:clip(document.title,256),text,controls,truncated:true,scope:'main_document',untrustedPageContent:true};
 const encoder=new TextEncoder();
 while(encoder.encode(JSON.stringify(result)).length>28000) {
  if(result.text.length)result.text=clip(result.text,Math.floor(result.text.length/2));
  else if(result.controls.length)result.controls.pop();
  else break;
 }
 return result;
})()"#;
