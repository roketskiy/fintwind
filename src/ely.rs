//! Ely GPUI Components integration.
//!
//! Two pieces live here so everything Ely needs from the shell stays in one
//! place: the asset merge (`Assets::before` serves Ely's embedded files first
//! and falls through to Fintwind's own icons and logos), and the window-level
//! focus scope Ely's Tab navigation expects.
//!
//! The focus scope is installed as a Base Root plugin rather than wrapped
//! around `Fintwind::render`: Root plugins decorate every window that mounts
//! the kit Root, so settings pages and any future kit-window adopt it for
//! free, and no product view has to carry an Ely type. `decorate` runs on the
//! outermost registered plugin last, which puts the scope around the kit's
//! overlay layer as well — exactly the "outside the window content" placement
//! Ely documents.

use ely_gpui_component::primitives::FocusScope;
use gpui::{App, Context, FocusHandle, IntoElement, ParentElement, Render, Window, div};
use gpui_kit::base::{Root, RootPlugin};

/// The outermost Ely focus scope around every kit window's content.
struct WindowFocusScope {
    /// Scope handle that takes focus back when the focused element leaves the
    /// tree; per window, created by the Root factory below.
    focus: FocusHandle,
}

impl RootPlugin for WindowFocusScope {
    fn decorate(
        &self,
        surface: gpui::AnyElement,
        _root: &Root,
        _window: &mut Window,
        _cx: &mut App,
    ) -> impl IntoElement {
        FocusScope::new(&self.focus).root().child(surface)
    }
}

impl Render for WindowFocusScope {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // The plugin's own view sits in the Root's overlay stack; this plugin
        // contributes nothing there — it only decorates the surface below.
        div()
    }
}

/// Register the focus-scope plugin. Must run before the first window opens:
/// a Root captures the plugin factories present at its construction.
pub fn init(cx: &mut App) {
    Root::register_plugin::<WindowFocusScope>(cx, |_window, cx| WindowFocusScope {
        focus: cx.focus_handle(),
    });
}
