use gpui::{hsla, rgb};

use super::{Theme, ThemeScheme};

impl Theme {
    pub(super) fn vs_code(is_dark: bool) -> Self {
        let mut theme = if is_dark { Self::dark() } else { Self::light() };
        theme.scheme = ThemeScheme::VsCode;
        if is_dark {
            // VS Code Dark Modern workbench surfaces; high-emphasis text and
            // status colors are adjusted for our smaller transcript controls.
            theme.canvas = rgb(0x1F1F1F).into();
            theme.sidebar = rgb(0x181818).into();
            theme.raised = rgb(0x252526).into();
            theme.composer = rgb(0x313131).into();
            theme.inset = rgb(0x181818).into();
            theme.terminal = rgb(0x1F1F1F).into();
            theme.text = rgb(0xCCCCCC).into();
            theme.text_secondary = rgb(0xB1B1B1).into();
            theme.text_tertiary = rgb(0x989898).into();
            theme.text_muted = rgb(0x929292).into();
            theme.text_ghost = rgb(0x999999).into();
            theme.accent = rgb(0x0078D4).into();
            theme.accent_text = rgb(0x70BAF3).into();
            theme.accent_fill = rgb(0x0078D4).into();
            theme.accent_focus = rgb(0x70BAF3).into();
            theme.on_accent = rgb(0xFFFFFF).into();
            theme.link = rgb(0x70BAF3).into();
            theme.success = rgb(0x2EA043).into();
            theme.success_text = rgb(0x73CD80).into();
            theme.danger = rgb(0xF85149).into();
            theme.danger_text = rgb(0xFF817A).into();
            theme.code_text = rgb(0xCE9178).into();
            theme.border = rgb(0x2B2B2B).into();
            theme.sidebar_border = theme.border;
            theme.border_strong = rgb(0x424242).into();
        } else {
            // Light Modern shares the same family, rather than a forced
            // dark palette when the system switches to light appearance.
            theme.canvas = rgb(0xFFFFFF).into();
            theme.sidebar = rgb(0xF8F8F8).into();
            theme.raised = rgb(0xF3F3F3).into();
            theme.composer = rgb(0xFFFFFF).into();
            theme.inset = rgb(0xF3F3F3).into();
            theme.text = rgb(0x3B3B3B).into();
            theme.text_secondary = rgb(0x595959).into();
            theme.text_tertiary = rgb(0x6B6B6B).into();
            theme.text_muted = rgb(0x656565).into();
            theme.text_ghost = rgb(0x696969).into();
            theme.accent = rgb(0x005FB8).into();
            theme.accent_text = theme.accent;
            theme.accent_fill = theme.accent;
            theme.accent_focus = theme.accent;
            theme.on_accent = rgb(0xFFFFFF).into();
            theme.link = theme.accent;
            theme.code_text = rgb(0xA31515).into();
            theme.border = rgb(0xE5E5E5).into();
            theme.sidebar_border = theme.border;
            theme.border_strong = rgb(0xC8C8C8).into();
        }
        theme.surface = theme.canvas;
        theme.sidebar_drag_background = theme.sidebar;
        theme.resize_handle = theme.accent;
        theme.gauge = theme.accent;
        theme
    }

    pub(super) fn codex(is_dark: bool) -> Self {
        let mut theme = if is_dark { Self::dark() } else { Self::light() };
        theme.scheme = ThemeScheme::Codex;
        // Codex-inspired desktop surfaces, not the CLI's terminal background.
        // The CLI provides accent values but inherits its terminal's surface.
        if is_dark {
            theme.canvas = rgb(0x181818).into();
            theme.sidebar = rgb(0x141414).into();
            theme.raised = rgb(0x222222).into();
            theme.composer = rgb(0x262626).into();
            theme.inset = rgb(0x111111).into();
            theme.terminal = theme.inset;
            theme.text = rgb(0xF2F2F2).into();
            theme.text_secondary = rgb(0xB9B9B9).into();
            theme.text_tertiary = rgb(0x9C9C9C).into();
            theme.text_muted = rgb(0x929292).into();
            theme.text_ghost = rgb(0x999999).into();
            theme.accent = rgb(0x339CFF).into();
            theme.accent_text = rgb(0x63A8F8).into();
            theme.accent_fill = theme.accent;
            theme.accent_focus = theme.accent_text;
            theme.on_accent = rgb(0x111827).into();
            theme.link = theme.accent_text;
            theme.code_text = rgb(0xA8CBF7).into();
            theme.border = rgb(0x303030).into();
            theme.border_strong = rgb(0x444444).into();
        } else {
            theme.canvas = rgb(0xFFFFFF).into();
            theme.sidebar = rgb(0xF7F7F7).into();
            theme.raised = rgb(0xF3F3F3).into();
            theme.composer = rgb(0xFFFFFF).into();
            theme.inset = rgb(0xEEEEEE).into();
            theme.text = rgb(0x0D0D0D).into();
            theme.text_secondary = rgb(0x555555).into();
            theme.text_tertiary = rgb(0x696969).into();
            theme.text_ghost = rgb(0x696969).into();
            theme.accent = rgb(0x1C64C8).into();
            theme.accent_text = theme.accent;
            theme.accent_fill = theme.accent;
            theme.accent_focus = theme.accent;
            theme.on_accent = rgb(0xFFFFFF).into();
            theme.link = theme.accent;
            theme.code_text = rgb(0x245C9E).into();
            theme.border = rgb(0xE1E1E1).into();
            theme.border_strong = rgb(0xC9C9C9).into();
        }
        theme.surface = theme.canvas;
        theme.sidebar_drag_background = theme.sidebar;
        theme.sidebar_border = theme.border;
        theme.resize_handle = theme.accent;
        theme.gauge = theme.accent;
        theme
    }

    pub(super) fn nord(is_dark: bool) -> Self {
        let mut theme = if is_dark { Self::dark() } else { Self::light() };
        theme.scheme = ThemeScheme::Nord;
        if is_dark {
            theme.canvas = rgb(0x2E3440).into();
            theme.sidebar = rgb(0x292F3A).into();
            theme.raised = rgb(0x3B4252).into();
            theme.composer = rgb(0x3B4252).into();
            theme.inset = rgb(0x252B36).into();
            theme.terminal = theme.inset;
            theme.text = rgb(0xECEFF4).into();
            theme.text_secondary = rgb(0xD8DEE9).into();
            theme.text_tertiary = rgb(0xC1CBD9).into();
            theme.text_muted = rgb(0xBAC5D4).into();
            theme.text_ghost = rgb(0x9BAABD).into();
            theme.accent = rgb(0x88C0D0).into();
            theme.accent_text = theme.accent;
            theme.accent_fill = theme.accent;
            theme.accent_focus = theme.accent;
            theme.on_accent = rgb(0x2E3440).into();
            theme.link = theme.accent;
            theme.warning = rgb(0xEBCB8B).into();
            theme.warning_text = theme.warning;
            theme.success = rgb(0xA3BE8C).into();
            theme.success_text = theme.success;
            theme.danger = rgb(0xBF616A).into();
            theme.danger_text = rgb(0xE5949B).into();
            theme.code_text = rgb(0xEBCB8B).into();
            theme.border = rgb(0x434C5E).into();
            theme.border_strong = rgb(0x4C566A).into();
            theme.inverse = rgb(0xECEFF4).into();
            theme.on_inverse = rgb(0x2E3440).into();
        } else {
            // Nord's Snow Storm is the light canvas; darker Polar Night and
            // Frost shades provide legible text and interaction colors.
            theme.canvas = rgb(0xECEFF4).into();
            theme.sidebar = rgb(0xE5E9F0).into();
            theme.raised = rgb(0xFFFFFF).into();
            theme.composer = rgb(0xFFFFFF).into();
            theme.inset = rgb(0xD8DEE9).into();
            theme.terminal = theme.raised;
            theme.text = rgb(0x2E3440).into();
            theme.text_secondary = rgb(0x434C5E).into();
            theme.text_tertiary = rgb(0x4C566A).into();
            theme.text_muted = rgb(0x4C566A).into();
            theme.text_ghost = rgb(0x58647A).into();
            theme.accent = rgb(0x5E81AC).into();
            theme.accent_text = rgb(0x385C89).into();
            theme.accent_fill = rgb(0x385C89).into();
            theme.accent_focus = theme.accent_text;
            theme.on_accent = rgb(0xFFFFFF).into();
            theme.link = theme.accent_text;
            theme.warning = rgb(0xA36B1D).into();
            theme.warning_text = rgb(0x82520E).into();
            theme.success = rgb(0x537637).into();
            theme.success_text = rgb(0x46652E).into();
            theme.danger = rgb(0xA44450).into();
            theme.danger_text = rgb(0x923641).into();
            theme.code_text = rgb(0x82520E).into();
            theme.border = rgb(0xCBD3E0).into();
            theme.border_strong = rgb(0xAFBACB).into();
            theme.inverse = rgb(0x2E3440).into();
            theme.on_inverse = rgb(0xECEFF4).into();
        }
        theme.surface = theme.canvas;
        theme.sidebar_drag_background = theme.sidebar;
        theme.sidebar_border = theme.border;
        theme.resize_handle = theme.accent_focus;
        theme.gauge = theme.accent;
        theme.favorite = theme.warning;
        theme.danger_soft = hsla(354.0 / 360.0, 0.5, 0.52, 0.12);
        theme
    }
}
