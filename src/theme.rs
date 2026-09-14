use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{App, Global, Hsla, Pixels, Window, WindowAppearance, hsla, px, rgb};

pub use fintwind_client::persistence::{DEFAULT_CODE_TEXT_SCALE, DEFAULT_UI_TEXT_SCALE};
pub use fintwind_client::theme::ThemePreference;

/// The user's UI text scale, stored as raw f32 bits. Atomics rather than a
/// GPUI global because line builders deep inside element trees read these on
/// every frame and threading a context into every closure that sizes text is
/// not worth it; relaxed ordering is enough for a presentation-only value.
static UI_TEXT_SCALE: AtomicU32 = AtomicU32::new(0);
/// The user's code text scale, stored as raw f32 bits.
static CODE_TEXT_SCALE: AtomicU32 = AtomicU32::new(0);

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

/// The text-size presets the appearance page offers, shared by the UI and
/// code settings. Multiplicative so every surface keeps its designed
/// proportions at any size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextSizePreset {
    Small,
    Default,
    Large,
    ExtraLarge,
}

impl TextSizePreset {
    pub const ALL: [Self; 4] = [Self::Small, Self::Default, Self::Large, Self::ExtraLarge];

    pub fn scale(self) -> f32 {
        match self {
            Self::Small => 0.9,
            Self::Default => 1.0,
            Self::Large => 1.1,
            Self::ExtraLarge => 1.25,
        }
    }

    pub fn label(self) -> String {
        let key = match self {
            Self::Small => "settings.text_size_small",
            Self::Default => "settings.text_size_default",
            Self::Large => "settings.text_size_large",
            Self::ExtraLarge => "settings.text_size_extra_large",
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
    pub canvas: Hsla,
    pub sidebar: Hsla,
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

    pub border: Hsla,
    pub border_strong: Hsla,
    pub sidebar_border: Hsla,

    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub text_ghost: Hsla,

    /// Brand coral. Logo, caret, live-activity pulses — nothing structural.
    pub accent: Hsla,
    pub resize_handle: Hsla,
    /// Meter fills in the usage panel. Quota-meter blue by convention;
    /// warning/danger take over as a lane fills.
    pub gauge: Hsla,

    /// Text-selection wash. Painted *under* the glyphs, so it stays
    /// translucent and deliberately reads as the familiar browser blue rather
    /// than as brand color.
    pub selection: Hsla,
    /// Inline `code` foreground and its rounded wash.
    pub code_text: Hsla,
    pub code_wash: Hsla,

    /// Light fill for primary buttons (send, allow), dark glyph on top.
    pub inverse: Hsla,
    pub on_inverse: Hsla,

    pub warning: Hsla,
    pub success: Hsla,
    pub favorite: Hsla,
    pub danger: Hsla,
    pub danger_soft: Hsla,
}

impl Theme {
    pub fn current(cx: &App) -> Self {
        if cx.has_global::<ActiveFintwindTheme>() {
            cx.global::<ActiveFintwindTheme>().0
        } else {
            Self::dark()
        }
    }

    pub fn dark() -> Self {
        Self {
            is_dark: true,
            canvas: rgb(0x1A1A1A).into(),
            sidebar: rgb(0x181818).into(),
            sidebar_drag_background: rgb(0x181818).into(),
            sidebar_item_background: hsla(0.0, 0.0, 0.941, 0.06),
            surface: rgb(0x1A1A1A).into(),
            raised: rgb(0x232323).into(),
            composer: rgb(0x212121).into(),
            inset: rgb(0x151515).into(),
            terminal: rgb(0x151515).into(),
            overlay: hsla(220.0 / 360.0, 0.10, 0.90, 0.05),
            overlay_strong: hsla(220.0 / 360.0, 0.10, 0.90, 0.09),

            border: hsla(220.0 / 360.0, 0.10, 0.90, 0.07),
            border_strong: hsla(220.0 / 360.0, 0.10, 0.90, 0.14),
            sidebar_border: hsla(126.93 / 360.0, 0.000_000_1, 0.16077, 1.0),

            text: rgb(0xE2E2E2).into(),
            text_secondary: rgb(0xA3A3A3).into(),
            text_tertiary: rgb(0x7D7D7D).into(),
            text_ghost: rgb(0x575757).into(),

            accent: rgb(0xD97757).into(),
            resize_handle: rgb(0x3B82F6).into(),
            gauge: rgb(0x3B82F6).into(),

            selection: hsla(211.0 / 360.0, 1.0, 0.50, 0.55),
            code_text: rgb(0xE0A882).into(),
            code_wash: hsla(220.0 / 360.0, 0.10, 0.90, 0.08),

            inverse: rgb(0xE7E9EC).into(),
            on_inverse: rgb(0x17181C).into(),

            warning: rgb(0xE0B36A).into(),
            success: rgb(0x62C987).into(),
            favorite: rgb(0xEAB308).into(),
            danger: rgb(0xE2726A).into(),
            danger_soft: hsla(4.0 / 360.0, 0.55, 0.63, 0.10),
        }
    }

    pub fn light() -> Self {
        Self {
            is_dark: false,
            canvas: rgb(0xFFFFFF).into(),
            sidebar: rgb(0xF9F9F9).into(),
            sidebar_drag_background: rgb(0xF9F9F9).into(),
            sidebar_item_background: hsla(0.0, 0.0, 0.078, 0.10),
            surface: rgb(0xFFFFFF).into(),
            raised: rgb(0xF4F4F4).into(),
            composer: rgb(0xFFFFFF).into(),
            inset: rgb(0xEFEFEF).into(),
            terminal: rgb(0xFFFFFF).into(),
            overlay: hsla(0.0, 0.0, 0.12, 0.05),
            overlay_strong: hsla(0.0, 0.0, 0.12, 0.09),

            border: hsla(0.0, 0.0, 0.12, 0.12),
            border_strong: hsla(0.0, 0.0, 0.12, 0.18),
            sidebar_border: hsla(0.0, 0.0, 0.078, 0.10),

            text: rgb(0x141414).into(),
            text_secondary: rgb(0x5C5C5C).into(),
            text_tertiary: rgb(0x8A8A8A).into(),
            text_ghost: rgb(0xA3A3A3).into(),

            accent: rgb(0xD97757).into(),
            resize_handle: rgb(0x2563EB).into(),
            gauge: rgb(0x2563EB).into(),

            selection: hsla(211.0 / 360.0, 1.0, 0.50, 0.35),
            code_text: rgb(0x9A5528).into(),
            code_wash: hsla(0.0, 0.0, 0.12, 0.07),

            inverse: rgb(0x202227).into(),
            on_inverse: rgb(0xF8F8F9).into(),

            warning: rgb(0xA66B20).into(),
            success: rgb(0x2F8F52).into(),
            favorite: rgb(0xCA8A04).into(),
            danger: rgb(0xC64A42).into(),
            danger_soft: hsla(4.0 / 360.0, 0.55, 0.52, 0.10),
        }
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

/// Resolve and publish the startup palette, before any window exists.
pub fn init(cx: &mut App) {
    let system_appearance = cx.window_appearance();
    let theme = if resolves_to_dark(ThemePreference::System, system_appearance) {
        Theme::dark()
    } else {
        Theme::light()
    };
    set_active_theme(theme, cx);
}

pub fn apply_theme_preference(preference: ThemePreference, window: &mut Window, cx: &mut App) {
    crate::platform::set_window_appearance(window, native_override(preference));
    let is_dark = resolves_to_dark(preference, cx.window_appearance());
    set_active_theme(
        if is_dark {
            Theme::dark()
        } else {
            Theme::light()
        },
        cx,
    );
    crate::platform::configure_sidebar_material(window, is_dark);
    window.refresh();
}
