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
}

impl ThemeScheme {
    pub const ALL: [Self; 4] = [Self::Default, Self::VsCode, Self::Codex, Self::Nord];

    pub fn label(self) -> String {
        let key = match self {
            Self::Default => "settings.scheme_default",
            Self::VsCode => "settings.scheme_vscode",
            Self::Codex => "settings.scheme_codex",
            Self::Nord => "settings.scheme_nord",
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
