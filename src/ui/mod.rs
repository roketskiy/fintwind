use crate::theme::ui_px;

use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Div, ElementId, Hsla, Img, InteractiveElement, Interactivity,
    KeyDownEvent, ParentElement, PathBuilder, Pixels, RenderOnce, SharedString, Stateful,
    StyleRefinement, Styled, Svg, Window, canvas, div, img, point, prelude::*, px, rgb, svg,
};

pub mod menu;
pub mod motion;
pub mod scrollbar;
pub mod text_field;
pub mod tooltip;

use crate::model::{ActivityKind, SessionStatus};
use crate::theme::Theme;

/// A monochrome icon from the embedded set, tinted via text color.
pub fn icon(path: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(path)
        .w(px(size))
        .h(px(size))
        .flex_none()
        .text_color(color)
}

/// A polychrome file icon rendered as an image so the SVG's authored colors
/// are preserved. GPUI's `svg()` element intentionally renders an alpha mask
/// tinted with one text color.
pub fn file_icon(path: &'static str, size: f32) -> Img {
    img(path).w(px(size)).h(px(size)).flex_none()
}

/// A compact ghost icon button: the only button shape outside the composer's
/// bespoke send control.
pub fn icon_button(id: impl Into<ElementId>, path: &'static str, theme: Theme) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(26.0))
        .rounded(px(7.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_default()
        .hover(|element| element.bg(theme.overlay))
        .active(|element| element.bg(theme.overlay_strong))
        .child(icon(path, 14.0, theme.text_tertiary))
}

/// Keeps a wheel gesture inside a scrollable nested in another scrollable
/// (activity output inside the transcript list, command output inside the
/// background-work page, a project group's sessions inside the sidebar),
/// matching AppKit: while the viewport under the pointer has overflow of its
/// own, the ancestor must not scroll it away. Call from an `on_scroll_wheel`
/// listener. The viewport's own scroll handler registers after user listeners,
/// so it has already consumed the delta when this stops the bubble; a viewport
/// whose content fits keeps chaining so short blocks do not dead-zone the
/// page. Stopping propagation also skips wheel listeners pushed earlier on the
/// same element, so fold any sibling wheel logic into the listener that calls
/// this.
pub fn contain_scroll<S>(surface: &S, cx: &mut App)
where
    S: scrollbar::Scrollable + ?Sized,
{
    if surface.max_offset() > px(0.5) {
        cx.stop_propagation();
    }
}

/// Add conventional mouse and keyboard activation to a focusable element.
pub trait ActivationExt: Sized {
    fn on_activation<E>(
        self,
        cx: &mut Context<E>,
        activate: impl Fn(&mut E, &mut Window, &mut Context<E>) + 'static,
    ) -> Self
    where
        E: 'static;
}

impl ActivationExt for Stateful<Div> {
    fn on_activation<E>(
        self,
        cx: &mut Context<E>,
        activate: impl Fn(&mut E, &mut Window, &mut Context<E>) + 'static,
    ) -> Self
    where
        E: 'static,
    {
        let activate = std::rc::Rc::new(activate);
        let click_activate = activate.clone();
        let key_activate = activate;
        self.on_click(cx.listener(move |this, _, window, cx| {
            click_activate(this, window, cx);
            cx.stop_propagation();
        }))
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            // Bare Enter/Space only. A modified chord belongs to whatever
            // command owns it, so a focused control must not swallow it —
            // this is the guard the hand-rolled settings toggles carried
            // before they moved onto this helper.
            if !event.keystroke.modifiers.modified()
                && matches!(event.keystroke.key.as_str(), "enter" | "space")
            {
                key_activate(this, window, cx);
                cx.stop_propagation();
            }
        }))
    }
}

/// The shared pill switch used by settings and automation forms.
///
/// `activate` is ignored while `disabled` is true, but the control remains in
/// the tab order so a pending operation does not move focus unexpectedly.
pub fn toggle_switch<E>(
    id: impl Into<ElementId>,
    on: bool,
    disabled: bool,
    theme: Theme,
    cx: &mut Context<E>,
    activate: impl Fn(&mut E, &mut Window, &mut Context<E>) + 'static,
) -> Stateful<Div>
where
    E: 'static,
{
    let base = div()
        .id(id)
        .tab_index(0)
        .focus_visible(|style| style.border_color(theme.accent))
        .w(px(42.0))
        .h(px(24.0))
        .p(px(3.0))
        .flex_none()
        .rounded_full()
        .cursor_default()
        .when(disabled, |element| element.opacity(0.55))
        .bg(if on { theme.inverse } else { theme.inset })
        .border_1()
        .border_color(if on {
            theme.inverse
        } else {
            theme.border_strong
        })
        .flex()
        .items_center()
        .when(on, |element| element.justify_end())
        .child(div().w(px(18.0)).h(px(18.0)).rounded_full().bg(if on {
            theme.on_inverse
        } else {
            theme.text_tertiary
        }));

    if disabled {
        base
    } else {
        base.on_activation(cx, activate)
    }
}

/// Neutral tint for a provider icon: near-ink in either theme, so the
/// letterform or brand mark carries the identity rather than a hue.
pub fn provider_color(theme: &Theme, _provider: &str) -> Hsla {
    if theme.is_dark {
        rgb(0xF3F3F3).into()
    } else {
        rgb(0x34363B).into()
    }
}

/// Icon for a model provider. OpenCode keeps its brand mark; every other
/// provider gets a letter glyph keyed by the first letter of its name, so a
/// provider configured later still reads as itself without a bespoke asset.
pub fn provider_icon(provider: &str) -> &'static str {
    if provider.trim().eq_ignore_ascii_case("opencode") {
        return "icons/provider-opencode.svg";
    }
    provider_letter_icon(provider)
}

/// Icon for a model: its company's brand mark when the model is recognizable,
/// otherwise the serving provider's icon. A provider often resells another
/// company's models — a router serving Grok is still Grok — so the mark shown
/// beside a model name is matched from the model's catalog id and display
/// name rather than taken from the provider.
pub fn model_icon(model_id: &str, model_name: &str, provider: &str) -> &'static str {
    model_company_icon(model_id, model_name).unwrap_or_else(|| provider_icon(provider))
}

/// Brand mark per model company, matched on distinctive model-id and display
/// name tokens. Checked top to bottom; list the more specific family first
/// when companies share a token. Companies without an embedded mark (Cohere,
/// 01.AI, StepFun, …) are absent on purpose and fall through to the provider
/// letter glyph.
const MODEL_COMPANY_MARKS: &[(&[&str], &str)] = &[
    (&["grok"], "icons/companies/xai.svg"),
    (&["claude"], "icons/companies/anthropic.svg"),
    (
        &[
            "gpt", "o1", "o3", "o4", "codex", "davinci", "chatgpt", "openai",
        ],
        "icons/companies/openai.svg",
    ),
    (&["gemini", "gemma", "google"], "icons/companies/google.svg"),
    (&["deepseek"], "icons/companies/deepseek.svg"),
    (
        &["qwen", "tongyi", "qwq", "qvq"],
        "icons/companies/qwen.svg",
    ),
    (&["kimi", "moonshot"], "icons/companies/moonshot.svg"),
    (
        &["glm", "chatglm", "zhipu", "zai"],
        "icons/companies/zai.svg",
    ),
    (&["llama", "meta"], "icons/companies/meta.svg"),
    (
        &[
            "mistral",
            "mixtral",
            "codestral",
            "pixtral",
            "magistral",
            "ministral",
            "devstral",
            "voxtral",
        ],
        "icons/companies/mistral.svg",
    ),
    (
        &["minimax", "hailuo", "abab"],
        "icons/companies/minimax.svg",
    ),
    (
        &["doubao", "bytedance", "ui-tars"],
        "icons/companies/bytedance.svg",
    ),
    (
        &["phi", "microsoft", "mai"],
        "icons/companies/microsoft.svg",
    ),
    (&["nova", "amazon"], "icons/companies/amazon.svg"),
    (
        &["sonar", "perplexity", "r1-1776"],
        "icons/companies/perplexity.svg",
    ),
];

/// The company mark for a model, or `None` when no company matches and the
/// caller should fall back to [`provider_icon`].
fn model_company_icon(model_id: &str, model_name: &str) -> Option<&'static str> {
    let haystack = format!("{} {}", model_id.trim(), model_name.trim()).to_ascii_lowercase();
    MODEL_COMPANY_MARKS
        .iter()
        .find(|(tokens, _)| tokens.iter().any(|token| contains_token(&haystack, token)))
        .map(|(_, icon)| *icon)
}

/// Substring match on token boundaries, so short marks like `o3` or `phi`
/// cannot match inside an unrelated identifier ("qwen3-o" or "domain" stay
/// unmatched while "o3-mini" and "phi-4" land).
fn contains_token(haystack: &str, token: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(token) {
        let start = from + offset;
        let end = start + token.len();
        let open = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let close = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if open && close {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Letter glyph for a provider name, drawn from its first ASCII letter so
/// digits, punctuation, and non-Latin prefixes still land on the letter that
/// leads the readable part of the name. Names without any ASCII letter fall
/// back to the generic mark.
fn provider_letter_icon(provider: &str) -> &'static str {
    let letter = provider
        .trim()
        .chars()
        .find(|ch| ch.is_ascii_alphabetic())
        .map(|ch| ch.to_ascii_uppercase());
    match letter {
        Some('A') => "icons/letters/a.svg",
        Some('B') => "icons/letters/b.svg",
        Some('C') => "icons/letters/c.svg",
        Some('D') => "icons/letters/d.svg",
        Some('E') => "icons/letters/e.svg",
        Some('F') => "icons/letters/f.svg",
        Some('G') => "icons/letters/g.svg",
        Some('H') => "icons/letters/h.svg",
        Some('I') => "icons/letters/i.svg",
        Some('J') => "icons/letters/j.svg",
        Some('K') => "icons/letters/k.svg",
        Some('L') => "icons/letters/l.svg",
        Some('M') => "icons/letters/m.svg",
        Some('N') => "icons/letters/n.svg",
        Some('O') => "icons/letters/o.svg",
        Some('P') => "icons/letters/p.svg",
        Some('Q') => "icons/letters/q.svg",
        Some('R') => "icons/letters/r.svg",
        Some('S') => "icons/letters/s.svg",
        Some('T') => "icons/letters/t.svg",
        Some('U') => "icons/letters/u.svg",
        Some('V') => "icons/letters/v.svg",
        Some('W') => "icons/letters/w.svg",
        Some('X') => "icons/letters/x.svg",
        Some('Y') => "icons/letters/y.svg",
        Some('Z') => "icons/letters/z.svg",
        _ => "icons/hexagon.svg",
    }
}

pub fn status_color(theme: &Theme, status: SessionStatus) -> Hsla {
    match status {
        SessionStatus::Idle => theme.text_ghost,
        SessionStatus::Connecting | SessionStatus::Working => theme.accent,
        SessionStatus::Waiting => theme.warning,
        SessionStatus::Failed => theme.danger,
    }
}

/// A rotating arc around a circular avatar. Slow stride: a sidebar full of
/// working sessions rebuilds its subtree per tick, and this motion survives
/// ~15 fps the way the old row spinner did.
pub fn spin_halo(color: Hsla, size: f32) -> AnyElement {
    const STROKE: f32 = 2.0;
    const ARC: f32 = 0.72;
    motion::pulse(Duration::from_millis(900), move |phase| {
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let center = bounds.center();
                let radius = px((size - STROKE) / 2.0);

                let mut track = PathBuilder::stroke(px(STROKE));
                track.move_to(point(center.x + radius, center.y));
                track.arc_to(
                    point(radius, radius),
                    px(0.0),
                    false,
                    true,
                    point(center.x - radius, center.y),
                );
                track.arc_to(
                    point(radius, radius),
                    px(0.0),
                    false,
                    true,
                    point(center.x + radius, center.y),
                );
                if let Ok(path) = track.build() {
                    window.paint_path(path, color.opacity(0.22));
                }

                let start = -std::f32::consts::FRAC_PI_2 + phase * std::f32::consts::TAU;
                let end = start + ARC * std::f32::consts::TAU;
                let mut arc = PathBuilder::stroke(px(STROKE));
                arc.move_to(point(
                    center.x + radius * start.cos(),
                    center.y + radius * start.sin(),
                ));
                arc.arc_to(
                    point(radius, radius),
                    px(0.0),
                    ARC > 0.5,
                    true,
                    point(center.x + radius * end.cos(), center.y + radius * end.sin()),
                );
                if let Ok(path) = arc.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .size(px(size))
        .into_any_element()
    })
    .every(2)
    .into_any_element()
}

#[cfg(test)]
pub fn activity_icon(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::Reasoning => "icons/sparkle.svg",
        ActivityKind::Command => "icons/terminal.svg",
        ActivityKind::FileChange => "icons/pencil.svg",
        ActivityKind::FileRead => "icons/file.svg",
        ActivityKind::FileSearch => "icons/search.svg",
        ActivityKind::FileList => "icons/folder.svg",
        ActivityKind::Search => "icons/search.svg",
        ActivityKind::Plan => "icons/list.svg",
        ActivityKind::Tool => "icons/wrench.svg",
    }
}

/// Per-tool glyph for a transcript activity row. Recognizable tool names get
/// a purpose-picked icon; everything else falls back to the category icon for
/// [`ActivityKind`], so an unfamiliar tool still reads as "a tool call".
#[cfg(test)]
pub fn activity_tool_icon(tool_name: &str, kind: ActivityKind) -> &'static str {
    let normalized = tool_name
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");
    if normalized.is_empty() {
        return activity_icon(kind);
    }
    // Claude-style MCP tools (`mcp__server__tool`) have arbitrary leaf names,
    // so the whole family shares one glyph instead of borrowing a built-in
    // tool's icon by coincidence.
    if normalized.contains("__") {
        return "icons/package.svg";
    }
    let leaf = normalized
        .rsplit([':', '.', '/'])
        .next()
        .unwrap_or(&normalized)
        .replace('_', "");
    match leaf.as_str() {
        "webfetch" | "fetch" | "urlfetch" | "openurl" | "browser" | "openbrowser" | "navigate" => {
            "icons/globe.svg"
        }
        "websearch" | "searchweb" | "search" => "icons/search.svg",
        "task" | "agent" | "newagent" | "spawnagent" | "dispatchagent" | "subagent" => {
            "icons/bot.svg"
        }
        "question" | "ask" | "askuser" | "askuserquestion" | "requestinput" | "userinput" => {
            "icons/info.svg"
        }
        "screenshot" | "computerscreenshot" | "computer" | "computeruse" => "icons/eye.svg",
        "applypatch" | "patch" => "icons/file-diff.svg",
        name if name.starts_with("github") => "icons/github.svg",
        name if name.starts_with("git") => "icons/git-branch.svg",
        _ => activity_icon(kind),
    }
}

pub fn activity_noun(kind: ActivityKind) -> (String, String) {
    match kind {
        ActivityKind::Reasoning => (tr!("activity.thought"), tr!("activity.thoughts")),
        ActivityKind::Command => (tr!("activity.command"), tr!("activity.commands")),
        ActivityKind::FileChange => (tr!("activity.file_edit"), tr!("activity.file_edits")),
        ActivityKind::FileRead => (tr!("activity.file_read"), tr!("activity.file_reads")),
        ActivityKind::FileSearch => (tr!("activity.file_search"), tr!("activity.file_searches")),
        ActivityKind::FileList => (tr!("activity.file_list"), tr!("activity.file_lists")),
        ActivityKind::Search => (tr!("activity.search"), tr!("activity.searches")),
        ActivityKind::Plan => (tr!("activity.plan_step"), tr!("activity.plan_steps")),
        ActivityKind::Tool => (tr!("activity.tool_call"), tr!("activity.tool_calls")),
    }
}

/// A compact chip used as a dropdown-menu trigger. `selected` is driven by the
/// menu's open state and renders as a soft fill.
#[derive(IntoElement)]
pub struct MenuChip {
    base: Stateful<Div>,
    icon: Option<(&'static str, Hsla)>,
    label: SharedString,
    caret: bool,
    outlined: bool,
    selected: bool,
    disabled: bool,
    height: Option<Pixels>,
    background: Option<Hsla>,
}

impl MenuChip {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: div().id(id),
            icon: None,
            label: SharedString::default(),
            caret: true,
            outlined: false,
            selected: false,
            disabled: false,
            height: None,
            background: None,
        }
    }

    /// Override the chip's fixed height, for rows whose controls share a
    /// different one.
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = Some(height);
        self
    }

    /// Fill behind an outlined chip. The default matches raised cards; a
    /// chip sitting directly on another surface passes that surface here so
    /// it doesn't read as a filled pill.
    pub fn background(mut self, background: Hsla) -> Self {
        self.background = Some(background);
        self
    }

    pub fn icon(mut self, path: &'static str, color: Hsla) -> Self {
        self.icon = Some((path, color));
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }

    pub fn outlined(mut self) -> Self {
        self.outlined = true;
        self
    }

    pub fn caret(mut self, caret: bool) -> Self {
        self.caret = caret;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Soft fill marking the chip as the open menu's trigger.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl Styled for MenuChip {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for MenuChip {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl ParentElement for MenuChip {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl RenderOnce for MenuChip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::current(cx);
        self.base
            .h(self
                .height
                .unwrap_or(if self.outlined { px(32.0) } else { px(28.0) }))
            .px(if self.outlined { px(11.0) } else { px(9.0) })
            .rounded(if self.outlined { px(8.0) } else { px(7.0) })
            .flex()
            .items_center()
            .gap(px(7.0))
            .text_size(ui_px(12.5))
            .line_height(ui_px(16.0))
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .when(self.outlined, |element| {
                element
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(self.background.unwrap_or(theme.raised))
            })
            .when(self.selected, |element| element.bg(theme.overlay))
            .when(!self.disabled, |element| {
                element
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
            })
            .when(self.disabled, |element| element.opacity(0.7))
            .when_some(self.icon, |element, (path, color)| {
                element.child(icon(path, 12.0, color))
            })
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text_secondary)
                    .child(self.label),
            )
            .when(self.caret, |element| {
                element.child(icon("icons/chevron-down.svg", 10.0, theme.text_ghost))
            })
    }
}

/// An inline, link-like dropdown trigger used for the project name in the
/// empty-state headline.
#[derive(IntoElement)]
pub struct ProjectNameSelector {
    base: Stateful<Div>,
    label: SharedString,
    selected: bool,
}

impl ProjectNameSelector {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            base: div().id(id),
            label: label.into(),
            selected: false,
        }
    }

    /// Emphasised underline while its menu is open.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl Styled for ProjectNameSelector {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for ProjectNameSelector {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl ParentElement for ProjectNameSelector {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(elements);
    }
}

impl RenderOnce for ProjectNameSelector {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::current(cx);
        let underline_color = if self.selected {
            theme.text_secondary
        } else {
            theme.text_tertiary
        };

        self.base
            .relative()
            .flex_none()
            .cursor_default()
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .child(self.label)
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let y = bounds.origin.y + bounds.size.height - px(0.5);
                        let mut builder =
                            PathBuilder::stroke(px(1.0)).dash_array(&[px(1.0), px(2.0)]);
                        builder.move_to(point(bounds.origin.x, y));
                        builder.line_to(point(bounds.origin.x + bounds.size.width, y));
                        if let Ok(line) = builder.build() {
                            window.paint_path(line, underline_color);
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_referenced_icon_is_embedded() {
        use crate::assets::Assets;
        use crate::model::ActivityKind;
        use gpui::AssetSource;

        let mut paths = vec![
            "icons/panel-left.svg",
            "icons/plus.svg",
            "icons/arrow-left.svg",
            "icons/arrow-right.svg",
            "icons/arrow-up.svg",
            "icons/stop.svg",
            "icons/check.svg",
            "icons/copy.svg",
            "icons/rewind.svg",
            "icons/fork.svg",
            "icons/git-branch.svg",
            "icons/chart-column.svg",
            "icons/chevron-down.svg",
            "icons/chevron-right.svg",
            "icons/chevron-up.svg",
            "icons/chevrons-up-down.svg",
            "icons/folder.svg",
            "icons/folder-new.svg",
            "icons/laptop.svg",
            "icons/file-diff.svg",
            "icons/globe.svg",
            "icons/alert.svg",
            "icons/lock.svg",
            "icons/lock-open.svg",
            "icons/star.svg",
            "icons/star-filled.svg",
            "icons/sparkle.svg",
            "icons/zap.svg",
            "icons/panel-right.svg",
            "icons/x.svg",
            "icons/bot.svg",
            "icons/rotate-cw.svg",
            "icons/package.svg",
            "icons/trash.svg",
            "icons/provider-opencode.svg",
        ];
        // Every branch of `provider_icon` must resolve to an embedded asset,
        // so a renamed letter glyph fails this test instead of vanishing from
        // the model chip at runtime.
        paths.push(provider_icon("opencode"));
        paths.push(provider_icon("——"));
        for letter in 'A'..='Z' {
            paths.push(provider_letter_icon(&letter.to_string()));
        }
        // Same for every company mark in `MODEL_COMPANY_MARKS`: one probe per
        // entry, so a renamed SVG fails here instead of vanishing from the
        // model chip at runtime.
        for (tokens, _) in MODEL_COMPANY_MARKS {
            paths.push(model_icon(tokens[0], tokens[0], "opencode"));
        }
        for kind in [
            ActivityKind::Reasoning,
            ActivityKind::Command,
            ActivityKind::FileChange,
            ActivityKind::FileRead,
            ActivityKind::FileSearch,
            ActivityKind::FileList,
            ActivityKind::Search,
            ActivityKind::Plan,
            ActivityKind::Tool,
        ] {
            paths.push(activity_icon(kind));
        }
        // Every branch of `activity_tool_icon` must resolve to an embedded
        // asset, so a renamed SVG fails this test instead of vanishing from
        // the transcript at runtime.
        for name in [
            "webfetch",
            "websearch",
            "task",
            "question",
            "mcp__deepwiki__read_wiki",
            "screenshot",
            "github_create_pull_request",
            "git_status",
            "apply_patch",
            "bash",
            "read",
            "glob",
            "ls",
            "todowrite",
            "totally_unknown_tool",
        ] {
            paths.push(activity_tool_icon(name, ActivityKind::Tool));
        }
        for path in paths {
            assert!(
                Assets.load(path).unwrap().is_some(),
                "missing embedded icon: {path}"
            );
        }
    }

    #[test]
    fn provider_icons_key_off_the_first_letter() {
        // The built-in provider keeps its brand mark regardless of case or
        // surrounding whitespace, mirroring `model_picker_provider_label`.
        assert_eq!(provider_icon("opencode"), "icons/provider-opencode.svg");
        assert_eq!(provider_icon(" OpenCode "), "icons/provider-opencode.svg");
        assert_eq!(provider_icon("anthropic"), "icons/letters/a.svg");
        assert_eq!(provider_icon("OpenAI"), "icons/letters/o.svg");
        assert_eq!(provider_icon("google-vertex"), "icons/letters/g.svg");
        assert_eq!(provider_icon("xai"), "icons/letters/x.svg");
        // The letter scan skips a leading non-letter so names like these
        // still land on their readable initial.
        assert_eq!(provider_icon("360gpt"), "icons/letters/g.svg");
        // No ASCII letter anywhere: generic mark.
        assert_eq!(provider_icon("  "), "icons/hexagon.svg");
        assert_eq!(provider_icon("云雾"), "icons/hexagon.svg");
    }

    #[test]
    fn model_icons_follow_the_company_behind_the_model() {
        // A router's slug never leaks into the mark: the company is matched
        // from the model id and display name, however the provider is named.
        assert_eq!(
            model_icon("some-router/grok-4.6", "Grok 4.6", "Some Router"),
            "icons/companies/xai.svg"
        );
        assert_eq!(
            model_icon("openrouter/claude-opus-4", "Claude Opus 4", "OpenRouter"),
            "icons/companies/anthropic.svg"
        );
        assert_eq!(
            model_icon("zhipuai/glm-4.6", "GLM 4.6", "Zhipu"),
            "icons/companies/zai.svg"
        );
        assert_eq!(
            model_icon("moonshotai/kimi-k2", "Kimi K2", "Moonshot"),
            "icons/companies/moonshot.svg"
        );
        // The match runs on either the id, the display name, or both.
        assert_eq!(
            model_company_icon("", "GPT-5").unwrap(),
            "icons/companies/openai.svg"
        );
        assert_eq!(
            model_company_icon("google/gemini-2.5-pro", "").unwrap(),
            "icons/companies/google.svg"
        );
        // Short tokens land on real models but not inside other words.
        assert_eq!(
            model_company_icon("openai/o3-mini", "").unwrap(),
            "icons/companies/openai.svg"
        );
        assert_eq!(
            model_company_icon("microsoft/phi-4", ""),
            Some("icons/companies/microsoft.svg")
        );
        assert_eq!(
            model_company_icon("qwen/qwen3-o", ""),
            Some("icons/companies/qwen.svg")
        );
        assert_eq!(model_company_icon("test/domain-x", ""), None);
        // No match: the provider's icon carries on.
        assert_eq!(
            model_icon("yi/yi-lightning", "Yi Lightning", "01.AI"),
            "icons/letters/a.svg"
        );
    }

    #[test]
    fn tool_icons_follow_the_tool_name_before_the_kind() {
        use crate::model::ActivityKind;

        assert_eq!(
            activity_tool_icon("webfetch", ActivityKind::Search),
            "icons/globe.svg"
        );
        assert_eq!(
            activity_tool_icon("Web Search", ActivityKind::Search),
            "icons/search.svg"
        );
        assert_eq!(
            activity_tool_icon("task", ActivityKind::Tool),
            "icons/bot.svg"
        );
        assert_eq!(
            activity_tool_icon("AskUserQuestion", ActivityKind::Tool),
            "icons/info.svg"
        );
        assert_eq!(
            activity_tool_icon("mcp__deepwiki__read_wiki", ActivityKind::Tool),
            "icons/package.svg"
        );
        assert_eq!(
            activity_tool_icon("computer/screenshot", ActivityKind::Tool),
            "icons/eye.svg"
        );
        assert_eq!(
            activity_tool_icon("apply_patch", ActivityKind::FileChange),
            "icons/file-diff.svg"
        );
        assert_eq!(
            activity_tool_icon("github_search_repos", ActivityKind::Tool),
            "icons/github.svg"
        );
        assert_eq!(
            activity_tool_icon("git_status", ActivityKind::Tool),
            "icons/git-branch.svg"
        );
        // Unrecognized names keep the category glyph, and empty titles
        // degrade to it too.
        assert_eq!(
            activity_tool_icon("create_thread", ActivityKind::Tool),
            "icons/wrench.svg"
        );
        assert_eq!(
            activity_tool_icon("read", ActivityKind::FileRead),
            "icons/file.svg"
        );
        assert_eq!(activity_tool_icon("", ActivityKind::Plan), "icons/list.svg");
    }
}
