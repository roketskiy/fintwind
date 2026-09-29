//! Process-neutral theme preference persisted in the desktop settings file.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

/// The color palette layered on top of the light/dark appearance.
///
/// Appearance answers "light or dark?" while this answers "which palette?".
/// Keeping the two preferences separate lets new palettes arrive without
/// changing the system-theme behavior or overloading `ThemePreference`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeScheme {
    #[default]
    Default,
    VsCode,
    Codex,
    Nord,
    Linear,
    Notion,
    One,
    Proof,
    Raycast,
    RosePine,
    Solarized,
    Vercel,
    VsCodePlus,
}

impl ThemeScheme {
    pub const ALL: [Self; 13] = [
        Self::Default,
        Self::VsCode,
        Self::Codex,
        Self::Nord,
        Self::Linear,
        Self::Notion,
        Self::One,
        Self::Proof,
        Self::Raycast,
        Self::RosePine,
        Self::Solarized,
        Self::Vercel,
        Self::VsCodePlus,
    ];

    pub fn label(self) -> String {
        let key = match self {
            Self::Default => "settings.scheme_default",
            Self::VsCode => "settings.scheme_vscode",
            Self::Codex => "settings.scheme_codex",
            Self::Nord => "settings.scheme_nord",
            Self::Linear => "settings.scheme_linear",
            Self::Notion => "settings.scheme_notion",
            Self::One => "settings.scheme_one",
            Self::Proof => "settings.scheme_proof",
            Self::Raycast => "settings.scheme_raycast",
            Self::RosePine => "settings.scheme_rose_pine",
            Self::Solarized => "settings.scheme_solarized",
            Self::Vercel => "settings.scheme_vercel",
            Self::VsCodePlus => "settings.scheme_vs_code_plus",
        };
        crate::i18n::translate(key)
    }
}

impl ThemePreference {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> String {
        match self {
            Self::System => crate::i18n::translate("settings.theme_system"),
            Self::Light => crate::i18n::translate("settings.theme_light"),
            Self::Dark => crate::i18n::translate("settings.theme_dark"),
        }
    }
}
