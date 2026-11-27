//! Shared settings-page layout primitives reused across Providers, MCP,
//! MCP Market, Skills, and SSH settings panels.

use gpui::{Div, ElementId, FontWeight, Hsla, ParentElement, SharedString, Stateful, Styled, div, prelude::*, px};

use crate::theme::{Theme, code_font_family, ui_px};
use crate::ui::{ModelMark, icon, model_mark_element};
use fintwind_protocol::tr;

pub fn section_label(theme: &Theme, label: String, first: bool) -> Div {
    div()
        .w_full()
        .pt(px(if first { 2.0 } else { 16.0 }))
        .pb(px(4.0))
        .px(px(9.0))
        .flex()
        .items_baseline()
        .text_size(ui_px(9.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_tertiary)
        .child(SharedString::from(label.to_uppercase()))
}

pub fn provider_tile(theme: &Theme, mark: ModelMark, active: bool) -> Div {
    div()
        .w(px(38.0))
        .h(px(38.0))
        .flex_none()
        .rounded(px(9.0))
        .bg(theme.overlay)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .opacity(if active { 1.0 } else { 0.45 })
                .child(model_mark_element(mark, 17.0, theme.text_secondary)),
        )
}

pub fn enabled_badge(theme: &Theme, accent: Hsla, enabled: bool) -> Div {
    div()
        .px(px(7.0))
        .py(px(2.0))
        .rounded_full()
        .text_size(ui_px(9.5))
        .when(enabled, |element| {
            element.text_color(accent).bg(accent.opacity(0.14))
        })
        .when(!enabled, |element| {
            element.text_color(theme.text_tertiary).bg(theme.overlay)
        })
        .child(if enabled {
            tr!("providers.enabled_badge")
        } else {
            tr!("providers.disabled_badge")
        })
}

pub fn small_pill(theme: &Theme, label: String, color: Option<Hsla>) -> Div {
    div()
        .px(px(6.0))
        .py(px(1.5))
        .rounded_full()
        .text_size(ui_px(9.0))
        .font_family(code_font_family())
        .text_color(color.unwrap_or(theme.text_tertiary))
        .bg(theme.overlay)
        .child(SharedString::from(label))
}

pub fn info_note(theme: &Theme, icon_path: &'static str, text: String) -> Div {
    div()
        .mt(px(12.0))
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .bg(theme.inset)
        .w_full()
        .min_w_0()
        .flex()
        .gap(px(8.0))
        .child(icon(icon_path, 12.0, theme.text_tertiary))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_size(ui_px(10.5))
                .line_height(ui_px(15.0))
                .text_color(theme.text_secondary)
                .child(SharedString::from(text)),
        )
}

pub fn form_hint(theme: &Theme, accent: Hsla, text: impl Into<SharedString>) -> gpui::AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(icon("icons/alert.svg", 12.0, accent))
        .child(
            div()
                .text_size(ui_px(10.5))
                .text_color(theme.text_secondary)
                .child(text.into()),
        )
        .into_any_element()
}

/// The standard outlined text button, ready for click and key handlers.
pub fn outline_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    icon_path: Option<&'static str>,
    theme: &Theme,
) -> Stateful<Div> {
    let mut button = div()
        .id(id)
        .tab_index(0)
        .focus_visible(|style| style.border_color(theme.accent_text))
        .h(px(30.0))
        // Trailing-icon button: the icon side carries the glyph's whitespace,
        // so it takes the smaller share.
        .pl(px(12.0))
        .pr(px(10.0))
        .rounded(px(7.0))
        .border_1()
        .border_color(theme.border_strong)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .cursor_default()
        .text_size(ui_px(12.0))
        .text_color(theme.text_secondary)
        .hover(|element| element.bg(theme.overlay))
        .active(|element| element.bg(theme.overlay_strong))
        .child(label.into());
    if let Some(icon_path) = icon_path {
        button = button.child(icon(icon_path, 12.5, theme.text_tertiary));
    }
    button
}

/// A compact icon-plus-label button for confirm/cancel affordances.
pub fn small_action_button(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    label: impl Into<SharedString>,
    color: Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .tab_index(0)
        .focus_visible(|style| style.border_color(theme.accent_text))
        .h(px(30.0))
        // Leading-icon button: icon side 2px tighter than the text side.
        .pl(px(8.0))
        .pr(px(10.0))
        .rounded(px(7.0))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.0))
        .cursor_default()
        .text_size(ui_px(12.0))
        .text_color(color)
        .hover(|element| element.bg(theme.overlay))
        .active(|element| element.bg(theme.overlay_strong))
        .child(icon(icon_path, 12.5, color))
        .child(label.into())
}

pub fn labeled_field(
    theme: &Theme,
    label: impl Into<SharedString>,
    field: impl gpui::IntoElement,
) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .text_size(ui_px(11.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(label.into()),
        )
        .child(field)
}
