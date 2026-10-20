use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use gpui::{
    App, BoxShadow, Global, Hsla, Pixels, SharedString, Window, WindowAppearance, hsla, px, rgb,
};
use parking_lot::Mutex;

mod palettes;

pub use fintwind_client::persistence::{
    DEFAULT_CODE_FONT_FAMILY, DEFAULT_CODE_TEXT_SCALE, DEFAULT_TRANSCRIPT_WIDTH,
    DEFAULT_UI_FONT_FAMILY, DEFAULT_UI_TEXT_SCALE, TRANSCRIPT_WIDTH_MAX, TRANSCRIPT_WIDTH_MIN,
    TRANSCRIPT_WIDTH_WIDE, normalize_transcript_width,
};
pub use fintwind_client::theme::{ThemePreference, ThemeScheme};

/// The user's UI text scale, stored as raw f32 bits. Atomics rather than a
/// GPUI global because line builders deep inside element trees read these on
/// every frame and threading a context into every closure that sizes text is
/// not worth it; relaxed ordering is enough for a presentation-only value.
static UI_TEXT_SCALE: AtomicU32 = AtomicU32::new(0);
/// The user's code text scale, stored as raw f32 bits.
static CODE_TEXT_SCALE: AtomicU32 = AtomicU32::new(0);
/// The transcript column's maximum content width in logical pixels, stored as
/// raw f32 bits. Read by the composer, transcript rows, and the navigation
/// rail so one setting keeps every centered column aligned.
static TRANSCRIPT_WIDTH: AtomicU32 = AtomicU32::new(0);
/// Current UI font family. Mutex rather than a GPUI global for the same
/// reason as the text scales: flatteners deep in the tree read this without
/// an App.
static UI_FONT_FAMILY: Mutex<Option<SharedString>> = Mutex::new(None);
/// Current code font family.
static CODE_FONT_FAMILY: Mutex<Option<SharedString>> = Mutex::new(None);
/// Bumped when either font family changes so cached text runs drop.
static FONT_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Publish the text-size preferences so every later frame renders with them.
/// Call again after the user changes the setting; the next paint reflows.
pub fn set_ui_text_scale(scale: f32) {
    UI_TEXT_SCALE.store(scale.to_bits(), Ordering::Relaxed);
}

pub fn ui_text_scale() -> f32 {
    let bits = UI_TEXT_SCALE.load(Ordering::Relaxed);
    if bits == 0 {
        DEFAULT_UI_TEXT_SCALE
    } else {
        f32::from_bits(bits)
    }
}

pub fn set_code_text_scale(scale: f32) {
    CODE_TEXT_SCALE.store(scale.to_bits(), Ordering::Relaxed);
}

pub fn code_text_scale() -> f32 {
    let bits = CODE_TEXT_SCALE.load(Ordering::Relaxed);
    if bits == 0 {
        DEFAULT_CODE_TEXT_SCALE
    } else {
        f32::from_bits(bits)
    }
}

/// Publish the transcript width preference so every later frame centers its
/// content column to it. The value is normalized into the presets' range and
/// snapped to the step, so the getter never reports an out-of-range width.
/// Call again after the user changes the setting.
pub fn set_transcript_width(width: f32) {
    TRANSCRIPT_WIDTH.store(
        normalize_transcript_width(width).to_bits(),
        Ordering::Relaxed,
    );
}

/// The current transcript column maximum content width, in the presets' range.
/// Falls back to the default before any setting is published.
pub fn transcript_width() -> f32 {
    let bits = TRANSCRIPT_WIDTH.load(Ordering::Relaxed);
    if bits == 0 {
        DEFAULT_TRANSCRIPT_WIDTH
    } else {
        f32::from_bits(bits)
    }
}

/// A UI-chrome text measurement that respects the user's text scale. Snap to
/// a quarter pixel so scaled text neither blurs nor makes layout jitter.
pub fn ui_px(size: f32) -> Pixels {
    px((size * ui_text_scale() * 4.0).round() / 4.0)
}

/// A code text measurement that respects the user's code text scale — code
/// blocks, editor panes, and the gutters that must line up with them.
pub fn code_px(size: f32) -> Pixels {
    px((size * code_text_scale() * 4.0).round() / 4.0)
}

fn stored_font(slot: &Mutex<Option<SharedString>>, default: &'static str) -> SharedString {
    slot.lock()
        .clone()
        .unwrap_or_else(|| SharedString::new_static(default))
}

fn publish_font(
    slot: &Mutex<Option<SharedString>>,
    family: impl Into<SharedString>,
    default: &'static str,
) {
    let family = family.into();
    let family = if family.is_empty() {
        SharedString::new_static(default)
    } else {
        family
    };
    let mut slot = slot.lock();
    if slot.as_ref() == Some(&family) {
        return;
    }
    *slot = Some(family);
    FONT_GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub fn set_ui_font_family(family: impl Into<SharedString>) {
    publish_font(&UI_FONT_FAMILY, family, DEFAULT_UI_FONT_FAMILY);
}

pub fn ui_font_family() -> SharedString {
    stored_font(&UI_FONT_FAMILY, DEFAULT_UI_FONT_FAMILY)
}

pub fn set_code_font_family(family: impl Into<SharedString>) {
    publish_font(&CODE_FONT_FAMILY, family, DEFAULT_CODE_FONT_FAMILY);
}

pub fn code_font_family() -> SharedString {
    stored_font(&CODE_FONT_FAMILY, DEFAULT_CODE_FONT_FAMILY)
}

pub fn font_generation() -> u64 {
    FONT_GENERATION.load(Ordering::Relaxed)
}

/// The text-size presets the appearance page offers, shared by the UI and
/// code settings. Multiplicative so every surface keeps its designed
/// proportions at any size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextSizePreset {
    Small,
    Default,
    Large,
    ExtraLarge,
    Huge,
}

impl TextSizePreset {
    pub const ALL: [Self; 5] = [
        Self::Small,
        Self::Default,
        Self::Large,
        Self::ExtraLarge,
        Self::Huge,
    ];

    pub fn scale(self) -> f32 {
        match self {
            Self::Small => 0.9,
            Self::Default => 1.0,
            Self::Large => 1.1,
            Self::ExtraLarge => 1.25,
            Self::Huge => 1.5,
        }
    }

    pub fn label(self) -> String {
        let key = match self {
            Self::Small => "settings.text_size_small",
            Self::Default => "settings.text_size_default",
            Self::Large => "settings.text_size_large",
            Self::ExtraLarge => "settings.text_size_extra_large",
            Self::Huge => "settings.text_size_huge",
        };
        crate::i18n::translate(key)
    }

    /// The preset a stored scale was picked from, so the chip can label a
    /// hand-edited setting with the closest option instead of an odd number.
    pub fn for_scale(scale: f32) -> Self {
        Self::ALL
            .into_iter()
            .min_by(|a, b| {
                (a.scale() - scale)
                    .abs()
                    .partial_cmp(&(b.scale() - scale).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(Self::Default)
    }
}

/// The transcript-width presets the appearance page offers. Values sit inside
/// the persisted range and are multiples of `TRANSCRIPT_WIDTH_STEP` so a
/// hand-edited width can be labelled with its closest preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TranscriptWidthPreset {
    Compact,
    Default,
    Wide,
    ExtraWide,
}

impl TranscriptWidthPreset {
    pub const ALL: [Self; 4] = [Self::Compact, Self::Default, Self::Wide, Self::ExtraWide];

    pub fn width(self) -> f32 {
        match self {
            Self::Compact => TRANSCRIPT_WIDTH_MIN,
            Self::Default => DEFAULT_TRANSCRIPT_WIDTH,
            Self::Wide => TRANSCRIPT_WIDTH_WIDE,
            Self::ExtraWide => TRANSCRIPT_WIDTH_MAX,
        }
    }

    pub fn label(self) -> String {
        let key = match self {
            Self::Compact => "settings.transcript_width_compact",
            Self::Default => "settings.transcript_width_default",
            Self::Wide => "settings.transcript_width_wide",
            Self::ExtraWide => "settings.transcript_width_extra_wide",
        };
        crate::i18n::translate(key)
    }

    /// The preset a stored width was picked from, so the chip can label a
    /// hand-edited setting with the closest option instead of an odd number.
    pub fn for_width(width: f32) -> Self {
        Self::ALL
            .into_iter()
            .min_by(|a, b| {
                (a.width() - width)
                    .abs()
                    .partial_cmp(&(b.width() - width).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(Self::Default)
    }
}

fn resolves_to_dark(preference: ThemePreference, system_appearance: WindowAppearance) -> bool {
    match preference {
        ThemePreference::System => matches!(
            system_appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ),
        ThemePreference::Light => false,
        ThemePreference::Dark => true,
    }
}

fn native_override(preference: ThemePreference) -> Option<bool> {
    match preference {
        ThemePreference::System => None,
        ThemePreference::Light => Some(false),
        ThemePreference::Dark => Some(true),
    }
}

/// Fintwind's visual language: graphite in dark, cool paper-white in light —
/// color is reserved for meaning. On macOS the sidebar's semantic tint is
/// installed as a native layer above Sidebar vibrancy; keeping this GPUI
/// surface clear avoids incorrectly accumulating the alpha of nested Metal
/// backgrounds. Selected, hovered, and pressed rows remain a thin neutral layer.
#[derive(Clone, Copy)]
pub struct Theme {
    pub is_dark: bool,
    pub scheme: ThemeScheme,
    pub canvas: Hsla,
    pub sidebar: Hsla,
    /// The L-shaped outer frame — the top bar and the mode rail — one step
    /// off the second column, so the layered layout reads without a heavier
    /// divider. Derived from `sidebar` in every palette.
    pub rail: Hsla,
    pub sidebar_drag_background: Hsla,
    pub sidebar_item_background: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub composer: Hsla,
    pub inset: Hsla,
    /// Terminal screen surface: paper-white in light mode, near-black in dark.
    pub terminal: Hsla,
    pub overlay: Hsla,
    pub overlay_strong: Hsla,
    /// Neutral emphasis for selected controls and checked switches.
    pub neutral_fill: Hsla,
    pub on_neutral: Hsla,

    pub border: Hsla,
    pub border_strong: Hsla,
    pub sidebar_border: Hsla,

    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    /// Low-emphasis text that still needs to remain readable on raised surfaces.
    pub text_muted: Hsla,
    pub text_ghost: Hsla,

    /// Theme accent for activity, focus and other interaction chrome.
    pub accent: Hsla,
    /// Accent used as text on neutral surfaces.
    pub accent_text: Hsla,
    /// Accent used for fills and tinted surfaces.
    pub accent_fill: Hsla,
    /// Accent used for focus indicators and other interaction chrome.
    pub accent_focus: Hsla,
    /// Foreground for content painted on `accent_fill`.
    pub on_accent: Hsla,
    /// Link text and underline color.
    pub link: Hsla,
    pub resize_handle: Hsla,
    /// Meter fills in the usage panel; warning/danger take over as a lane fills.
    pub gauge: Hsla,

    /// Text-selection wash. Painted *under* the glyphs, so it stays
    /// translucent and deliberately reads as the familiar browser blue rather
    /// than as brand color.
    pub selection: Hsla,
    /// Inline `code` rounded wash.
    pub code_wash: Hsla,
    /// Syntax colors for code blocks and the code editor, one per token
    /// class. Every palette family supplies both appearances so transcript
    /// highlighting no longer needs per-scheme branches.
    pub code_keyword: Hsla,
    pub code_literal: Hsla,
    pub code_string: Hsla,
    pub code_number: Hsla,
    pub code_type: Hsla,
    pub code_function: Hsla,

    /// Light fill for primary buttons (send, allow), dark glyph on top.
    pub inverse: Hsla,
    pub on_inverse: Hsla,

    pub warning: Hsla,
    pub warning_text: Hsla,
    pub success: Hsla,
    pub success_text: Hsla,
    pub favorite: Hsla,
    pub danger: Hsla,
    pub danger_text: Hsla,
    pub danger_soft: Hsla,
}

impl Theme {
    pub fn sidebar_shadow(&self) -> Vec<BoxShadow> {
        vec![
            BoxShadow::new(
                px(-1.0),
                px(0.0),
                hsla(0.0, 0.0, 0.0, if self.is_dark { 0.14 } else { 0.04 }),
            )
            .blur_radius(px(4.0)),
        ]
    }

    /// A contact shadow and a soft cast shadow lift the workspace above navigation.
    pub fn workspace_shadow(&self) -> Vec<BoxShadow> {
        let (contact, ambient) = if self.is_dark {
            (0.20, 0.14)
        } else {
            (0.08, 0.045)
        };
        vec![
            BoxShadow::new(px(-1.0), px(0.0), hsla(0.0, 0.0, 0.0, contact)).blur_radius(px(2.0)),
            BoxShadow::new(px(-2.0), px(0.0), hsla(0.0, 0.0, 0.0, ambient)).blur_radius(px(8.0)),
        ]
    }

    /// A quiet downward lift for the context panel's cards, in the same
    /// two-layer idiom as the sidebar and workspace shadows but smaller.
    pub fn card_shadow(&self) -> Vec<BoxShadow> {
        let (key, ambient) = if self.is_dark {
            (0.16, 0.09)
        } else {
            (0.05, 0.03)
        };
        vec![
            BoxShadow::new(px(0.0), px(1.0), hsla(0.0, 0.0, 0.0, key)).blur_radius(px(2.0)),
            BoxShadow::new(px(0.0), px(3.0), hsla(0.0, 0.0, 0.0, ambient)).blur_radius(px(7.0)),
        ]
    }

    pub fn current(cx: &App) -> Self {
        if cx.has_global::<ActiveFintwindTheme>() {
            cx.global::<ActiveFintwindTheme>().0
        } else {
            Self::dark()
        }
    }

    pub fn dark() -> Self {
        Self::for_scheme(ThemeScheme::Default, true)
    }

    fn base_dark() -> Self {
        Self {
            is_dark: true,
            scheme: ThemeScheme::Default,
            canvas: rgb(0x222222).into(),
            sidebar: rgb(0x1A1A1A).into(),
            rail: rgb(0x141414).into(),
            sidebar_drag_background: rgb(0x1A1A1A).into(),
            sidebar_item_background: hsla(0.0, 0.0, 0.941, 0.06),
            surface: rgb(0x222222).into(),
            raised: rgb(0x2B2B2B).into(),
            composer: rgb(0x282828).into(),
            inset: rgb(0x151515).into(),
            terminal: rgb(0x151515).into(),
            overlay: hsla(220.0 / 360.0, 0.10, 0.90, 0.05),
            overlay_strong: hsla(220.0 / 360.0, 0.10, 0.90, 0.09),
            neutral_fill: rgb(0xD6D6D6).into(),
            on_neutral: rgb(0x1A1A1A).into(),

            border: hsla(220.0 / 360.0, 0.10, 0.90, 0.07),
            border_strong: hsla(220.0 / 360.0, 0.10, 0.90, 0.14),
            sidebar_border: hsla(126.93 / 360.0, 0.000_000_1, 0.16077, 1.0),

            text: rgb(0xE2E2E2).into(),
            text_secondary: rgb(0xA3A3A3).into(),
            text_tertiary: rgb(0x929292).into(),
            text_muted: rgb(0x929292).into(),
            text_ghost: rgb(0x575757).into(),

            accent: rgb(0xD97757).into(),
            accent_text: rgb(0xD97757).into(),
            accent_fill: rgb(0xD97757).into(),
            accent_focus: rgb(0xD97757).into(),
            on_accent: rgb(0x201814).into(),
            link: rgb(0x8ACFF8).into(),
            resize_handle: rgb(0x3B82F6).into(),
            gauge: rgb(0x3B82F6).into(),

            selection: hsla(211.0 / 360.0, 1.0, 0.50, 0.55),
            code_wash: hsla(220.0 / 360.0, 0.10, 0.90, 0.08),
            code_keyword: rgb(0xC98BC0).into(),
            code_literal: rgb(0xD9A05B).into(),
            code_string: rgb(0x94C08A).into(),
            code_number: rgb(0xD9A05B).into(),
            code_type: rgb(0x8FB8D9).into(),
            code_function: rgb(0x8FB8D9).into(),

            inverse: rgb(0xE7E9EC).into(),
            on_inverse: rgb(0x17181C).into(),

            warning: rgb(0xE0B36A).into(),
            warning_text: rgb(0xE0B36A).into(),
            success: rgb(0x62C987).into(),
            success_text: rgb(0x62C987).into(),
            favorite: rgb(0xEAB308).into(),
            danger: rgb(0xE2726A).into(),
            danger_text: rgb(0xE2726A).into(),
            danger_soft: hsla(4.0 / 360.0, 0.55, 0.63, 0.10),
        }
    }

    pub fn light() -> Self {
        Self::for_scheme(ThemeScheme::Default, false)
    }

    fn base_light() -> Self {
        Self {
            is_dark: false,
            scheme: ThemeScheme::Default,
            canvas: rgb(0xF7F7F7).into(),
            sidebar: rgb(0xF3F3F3).into(),
            rail: rgb(0xEAEAEA).into(),
            sidebar_drag_background: rgb(0xF3F3F3).into(),
            sidebar_item_background: hsla(0.0, 0.0, 0.078, 0.10),
            surface: rgb(0xF7F7F7).into(),
            raised: rgb(0xF4F4F4).into(),
            composer: rgb(0xFAFAFA).into(),
            inset: rgb(0xEFEFEF).into(),
            terminal: rgb(0xFFFFFF).into(),
            overlay: hsla(0.0, 0.0, 0.12, 0.05),
            overlay_strong: hsla(0.0, 0.0, 0.12, 0.09),
            neutral_fill: rgb(0x343434).into(),
            on_neutral: rgb(0xFFFFFF).into(),

            border: hsla(0.0, 0.0, 0.12, 0.12),
            border_strong: hsla(0.0, 0.0, 0.12, 0.18),
            sidebar_border: hsla(0.0, 0.0, 0.078, 0.10),

            text: rgb(0x141414).into(),
            text_secondary: rgb(0x5C5C5C).into(),
            text_tertiary: rgb(0x696969).into(),
            text_muted: rgb(0x686868).into(),
            text_ghost: rgb(0xA3A3A3).into(),

            accent: rgb(0xB25A39).into(),
            accent_text: rgb(0xA75030).into(),
            accent_fill: rgb(0xB25A39).into(),
            accent_focus: rgb(0xB25A39).into(),
            on_accent: rgb(0xFFFFFF).into(),
            link: rgb(0x1769AA).into(),
            resize_handle: rgb(0x2563EB).into(),
            gauge: rgb(0x2563EB).into(),

            selection: hsla(211.0 / 360.0, 1.0, 0.50, 0.35),
            code_wash: hsla(0.0, 0.0, 0.12, 0.07),
            code_keyword: rgb(0x9A4B92).into(),
            code_literal: rgb(0x9A6019).into(),
            code_string: rgb(0x3F7A36).into(),
            code_number: rgb(0x9A6019).into(),
            code_type: rgb(0x2F6690).into(),
            code_function: rgb(0x2F6690).into(),

            inverse: rgb(0x202227).into(),
            on_inverse: rgb(0xF8F8F9).into(),

            warning: rgb(0xA66B20).into(),
            warning_text: rgb(0x8A5A00).into(),
            success: rgb(0x2F8F52).into(),
            success_text: rgb(0x236B3C).into(),
            favorite: rgb(0xCA8A04).into(),
            danger: rgb(0xC64A42).into(),
            danger_text: rgb(0xB43A34).into(),
            danger_soft: hsla(4.0 / 360.0, 0.55, 0.52, 0.10),
        }
    }

    /// Resolve a palette family against the current light/dark appearance.
    ///
    /// New families use a light/dark base and override the semantics they
    /// restyle; the original presets remain explicit (see `palettes`).
    pub fn for_scheme(scheme: ThemeScheme, is_dark: bool) -> Self {
        let theme = match scheme {
            ThemeScheme::Default => {
                if is_dark {
                    Self::base_dark()
                } else {
                    Self::base_light()
                }
            }
            ThemeScheme::VsCode => Self::vs_code(is_dark),
            ThemeScheme::Codex => Self::codex(is_dark),
            ThemeScheme::Nord => Self::nord(is_dark),
            ThemeScheme::Linear => Self::linear(is_dark),
            ThemeScheme::Notion => Self::notion(is_dark),
            ThemeScheme::One => Self::one(is_dark),
            ThemeScheme::Proof => Self::proof(is_dark),
            ThemeScheme::Raycast => Self::raycast(is_dark),
            ThemeScheme::RosePine => Self::rose_pine(is_dark),
            ThemeScheme::Solarized => Self::solarized(is_dark),
            ThemeScheme::Vercel => Self::vercel(is_dark),
            ThemeScheme::VsCodePlus => Self::vs_code_plus(is_dark),
        };
        theme.with_muted_accents()
    }

    fn with_muted_accents(mut self) -> Self {
        for color in [
            &mut self.accent,
            &mut self.accent_text,
            &mut self.accent_fill,
            &mut self.accent_focus,
            &mut self.link,
            &mut self.resize_handle,
            &mut self.gauge,
            &mut self.selection,
        ] {
            color.s *= 0.75;
        }
        self
    }
}

#[derive(Clone, Copy)]
struct ActiveFintwindTheme(Theme);

impl Global for ActiveFintwindTheme {}

/// Publish the resolved palette. [`Theme::current`] reads it back from the
/// global, which is how every view gets its colors.
fn set_active_theme(theme: Theme, cx: &mut App) {
    cx.set_global(ActiveFintwindTheme(theme));
}

/// Project the resolved Fintwind palette onto gpui-kit's global theme, so kit
/// components (and the overlays its Root plugin hosts) follow Fintwind's
/// scheme, fonts and text scale instead of the library defaults.
///
/// One-way only: Fintwind's `Theme` stays the single source of truth. Called
/// at startup and when the user or the system changes the appearance — never
/// from render, because both write paths refresh every window.
pub fn sync_gpui_kit_theme(cx: &mut App) {
    use gpui_kit::component::{Theme as KitTheme, ThemeMode};

    let theme = Theme::current(cx);
    let mode = if theme.is_dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    // Mode first, colors second: switching the mode loads the registered
    // theme of that mode and replaces the colors, so the projection below
    // must land after it (gpui-kit's documented order).
    KitTheme::change(mode, None, cx);
    KitTheme::update(cx, |kit| {
        let c = &mut kit.colors;
        c.background = theme.canvas;
        c.foreground = theme.text;
        c.accent = theme.accent;
        c.accent_foreground = theme.on_accent;
        c.border = theme.border_strong;
        c.input = theme.composer;
        c.ring = theme.neutral_fill;
        c.switch = theme
            .neutral_fill
            .opacity(if theme.is_dark { 0.4 } else { 0.55 });
        c.switch_thumb = theme.on_neutral;
        c.muted = theme.raised;
        c.muted_foreground = theme.text_secondary;
        c.secondary = theme.raised;
        c.secondary_foreground = theme.text;
        c.secondary_hover = theme.sidebar_item_background;
        c.popover = theme.raised;
        c.popover_foreground = theme.text;
        c.overlay = theme.overlay;
        c.link = theme.link;
        c.link_active = theme.link;
        c.link_hover = theme.link;
        c.selection = theme.selection;
        c.danger = theme.danger;
        c.danger_foreground = theme.danger_text;
        c.danger_hover = theme.danger_soft;
        c.warning = theme.warning;
        c.warning_foreground = theme.warning_text;
        c.success = theme.success;
        c.success_foreground = theme.success_text;
        // Fintwind's primary buttons are a light fill with a dark glyph in
        // both appearances (`inverse`), so project that pair rather than the
        // accent the kit would otherwise reuse.
        c.primary = theme.inverse;
        c.primary_foreground = theme.on_inverse;
        c.primary_hover = theme.inverse;
        c.primary_active = theme.inverse;
        c.sidebar = theme.sidebar;
        c.sidebar_foreground = theme.text;
        c.sidebar_border = theme.sidebar_border;
        c.sidebar_accent = theme.sidebar_item_background;
        c.sidebar_accent_foreground = theme.text;
        c.title_bar = theme.rail;
        c.title_bar_border = theme.border;
        c.scrollbar = theme.overlay_strong;
        c.scrollbar_thumb = theme.overlay_strong;
        c.scrollbar_thumb_hover = theme.border_strong;

        kit.font_family = ui_font_family();
        kit.mono_font_family = code_font_family();
        // The kit Root plugin pushes `font_size` into `window.set_rem_size`
        // every frame, which is how kit components follow Fintwind's text
        // scale; Fintwind's own chrome keeps measuring through `ui_px`.
        kit.font_size = px(16.0 * ui_text_scale());
        kit.mono_font_size = px(13.0 * code_text_scale());
    });
}

/// Project the resolved Fintwind palette onto Ely's global theme, so Ely
/// components render in Fintwind's scheme instead of the library defaults
/// (the performance viewer consumes the projected border, text, hover, and
/// selection colors through Ely's own primitives).
///
/// Colors travel through Ely's official custom-palette channel
/// (`Theme::set_palette`) for the mode that is showing, so a later Ely mode
/// switch keeps resolving to the projected palette instead of the library
/// default. `set_palette` starts a color fade; the `set_mode_now` right
/// after it advances Ely's transition counter, which cancels that fade on
/// its first tick and lands the colors immediately — no visible animation,
/// so reduce-motion is respected and nothing refreshes repeatedly. Font
/// tokens are not colors and go through `Theme::update`, which is their
/// documented channel.
///
/// One-way only: Fintwind's `Theme` stays the single source of truth. Same
/// rhythm as [`sync_gpui_kit_theme`]: startup and appearance changes, never
/// from render.
pub fn sync_ely_theme(cx: &mut App) {
    use ely_gpui_component::theme::{
        ActiveTheme as _, Mode as ElyMode, Palette as ElyPalette, Theme as ElyTheme,
    };

    // Ely registers its theme global in its own init; the browser PoC host
    // runs the Fintwind theme startup without that init, and there is
    // nothing to project onto there.
    if !cx.has_global::<ElyTheme>() {
        return;
    }
    let theme = Theme::current(cx);
    let mode = if theme.is_dark {
        ElyMode::Dark
    } else {
        ElyMode::Light
    };
    // Project onto the corresponding Ely baseline, never onto a palette
    // captured earlier: each sync starts from Ely's own mode palette so no
    // stale custom value can leak through a scheme change.
    let mut palette = match mode {
        ElyMode::Light => ElyPalette::light(false),
        ElyMode::Dark => ElyPalette::dark(false),
    };
    let colors = &mut palette;
    colors.bg = theme.canvas;
    colors.surface = theme.raised;
    colors.hover = theme.overlay;
    colors.active = theme.overlay_strong;
    colors.border = theme.border_strong;
    colors.fg = theme.text;
    colors.fg_muted = theme.text_secondary;
    colors.fg_subtle = theme.text_tertiary;
    colors.accent = theme.accent;
    colors.focus = theme.accent_focus;
    colors.link = theme.link;
    colors.selection = theme.selection;
    if cx.theme().palette() != palette {
        ElyTheme::set_palette(mode, Some(palette), cx);
    }
    // Advances Ely's transition counter: cancels the fade `set_palette` may
    // have started and lands the colors on the custom palette at once.
    ElyTheme::set_mode_now(mode, cx);
    let (ui_font, mono_font) = (ui_font_family(), code_font_family());
    ElyTheme::update(cx, |ely| {
        ely.font_family = ui_font;
        ely.mono_family = mono_font;
    });
}

/// Resolve and publish the startup palette, before any window exists.
pub fn init(cx: &mut App) {
    let system_appearance = cx.window_appearance();
    let theme = if resolves_to_dark(ThemePreference::System, system_appearance) {
        Theme::dark()
    } else {
        Theme::light()
    };
    set_active_theme(theme, cx);
    sync_gpui_kit_theme(cx);
    sync_ely_theme(cx);
}

pub fn apply_theme_preference(
    preference: ThemePreference,
    scheme: ThemeScheme,
    window: &mut Window,
    cx: &mut App,
) {
    crate::platform::set_window_appearance(window, native_override(preference));
    let is_dark = resolves_to_dark(preference, cx.window_appearance());
    set_active_theme(Theme::for_scheme(scheme, is_dark), cx);
    crate::platform::configure_sidebar_material(window, is_dark);
    sync_gpui_kit_theme(cx);
    sync_ely_theme(cx);
    window.refresh();
}
