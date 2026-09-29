// Family seeds are deliberately written as full struct literals ending in
// `..PaletteSeed::default()`. The update is what makes a future semantic field
// mean "inherit the base palette" for every family at once, instead of
// breaking all eighteen seeds and forcing a decision in each of them.
#![allow(clippy::needless_update)]

use gpui::{hsla, rgb};

use super::{Theme, ThemeScheme};

/// One palette family expressed only as the semantics it actually restyles.
///
/// Each family layers a seed on top of the light/dark base palette, so a
/// family records its identity — surfaces, the text ramp, accent, status and
/// syntax colors — instead of restating all ~60 theme fields. Anything left
/// `None` keeps the base value, which is how the washes (`overlay`,
/// `selection`, `code_wash`) and the hover layers stay neutral across every
/// family, and why the primary button keeps its neutral `inverse` fill.
/// Surfaces Fintwind derives uniformly (`surface`,
/// `sidebar_drag_background`, `sidebar_border`, `resize_handle`, `gauge`) are
/// computed in [`Theme::family`] instead of being stored per family.
///
/// Values are plain `0xRRGGBB` literals; hex sources are noted per family.
#[derive(Clone, Copy, Default)]
struct PaletteSeed {
    // Surfaces
    canvas: Option<u32>,
    sidebar: Option<u32>,
    raised: Option<u32>,
    composer: Option<u32>,
    inset: Option<u32>,
    terminal: Option<u32>,
    // Hairlines
    border: Option<u32>,
    border_strong: Option<u32>,
    // Text ramp
    text: Option<u32>,
    text_secondary: Option<u32>,
    text_tertiary: Option<u32>,
    text_muted: Option<u32>,
    text_ghost: Option<u32>,
    // Accent family
    accent: Option<u32>,
    accent_text: Option<u32>,
    accent_fill: Option<u32>,
    accent_focus: Option<u32>,
    on_accent: Option<u32>,
    link: Option<u32>,
    // Status
    warning: Option<u32>,
    warning_text: Option<u32>,
    success: Option<u32>,
    success_text: Option<u32>,
    favorite: Option<u32>,
    danger: Option<u32>,
    danger_text: Option<u32>,
    // Inline code and the syntax tokens shared with code blocks
    code_text: Option<u32>,
    code_keyword: Option<u32>,
    code_literal: Option<u32>,
    code_string: Option<u32>,
    code_number: Option<u32>,
    code_type: Option<u32>,
    code_function: Option<u32>,
}

fn color(hex: u32) -> gpui::Hsla {
    rgb(hex).into()
}

impl Theme {
    /// Build one family palette: the light/dark base plus the seed's
    /// overrides. `on_accent` is paired with both `accent_fill` and
    /// `accent_text`: some colored controls use the latter as their fill.
    fn family(scheme: ThemeScheme, is_dark: bool, seed: PaletteSeed) -> Self {
        let mut theme = if is_dark { Self::dark() } else { Self::light() };
        theme.scheme = scheme;

        theme.canvas = seed.canvas.map_or(theme.canvas, color);
        theme.sidebar = seed.sidebar.map_or(theme.sidebar, color);
        theme.raised = seed.raised.map_or(theme.raised, color);
        theme.composer = seed.composer.map_or(theme.composer, color);
        theme.inset = seed.inset.map_or(theme.inset, color);
        theme.terminal = seed.terminal.map_or(theme.terminal, color);

        theme.border = seed.border.map_or(theme.border, color);
        theme.border_strong = seed.border_strong.map_or(theme.border_strong, color);

        theme.text = seed.text.map_or(theme.text, color);
        theme.text_secondary = seed.text_secondary.map_or(theme.text_secondary, color);
        theme.text_tertiary = seed.text_tertiary.map_or(theme.text_tertiary, color);
        theme.text_muted = seed.text_muted.map_or(theme.text_muted, color);
        theme.text_ghost = seed.text_ghost.map_or(theme.text_ghost, color);

        theme.accent = seed.accent.map_or(theme.accent, color);
        theme.accent_text = seed.accent_text.map_or(theme.accent_text, color);
        theme.accent_fill = seed.accent_fill.map_or(theme.accent_fill, color);
        theme.accent_focus = seed.accent_focus.map_or(theme.accent_focus, color);
        theme.on_accent = seed.on_accent.map_or(theme.on_accent, color);
        theme.link = seed.link.map_or(theme.link, color);

        theme.warning = seed.warning.map_or(theme.warning, color);
        theme.warning_text = seed.warning_text.map_or(theme.warning_text, color);
        theme.success = seed.success.map_or(theme.success, color);
        theme.success_text = seed.success_text.map_or(theme.success_text, color);
        theme.favorite = seed.favorite.map_or(theme.favorite, color);
        theme.danger = seed.danger.map_or(theme.danger, color);
        theme.danger_text = seed.danger_text.map_or(theme.danger_text, color);

        theme.code_text = seed.code_text.map_or(theme.code_text, color);
        theme.code_keyword = seed.code_keyword.map_or(theme.code_keyword, color);
        theme.code_literal = seed.code_literal.map_or(theme.code_literal, color);
        theme.code_string = seed.code_string.map_or(theme.code_string, color);
        theme.code_number = seed.code_number.map_or(theme.code_number, color);
        theme.code_type = seed.code_type.map_or(theme.code_type, color);
        theme.code_function = seed.code_function.map_or(theme.code_function, color);

        // Uniformly derived chrome: the transcript reads as one surface while
        // the sidebar stays its own, and interaction chrome tracks the accent.
        theme.surface = theme.canvas;
        theme.sidebar_drag_background = theme.sidebar;
        theme.sidebar_border = theme.border;
        theme.resize_handle = theme.accent_focus;
        theme.gauge = theme.accent;
        theme
    }

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
            // Dark+ token colors (VS Code theme reference).
            theme.code_keyword = rgb(0xC586C0).into();
            theme.code_literal = rgb(0xB5CEA8).into();
            theme.code_string = rgb(0xCE9178).into();
            theme.code_number = rgb(0xB5CEA8).into();
            theme.code_type = rgb(0x4EC9B0).into();
            theme.code_function = rgb(0xDCDCAA).into();
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
            // Light+ token colors (VS Code theme reference).
            theme.code_keyword = rgb(0xAF00DB).into();
            theme.code_literal = rgb(0x087044).into();
            theme.code_string = rgb(0xA31515).into();
            theme.code_number = rgb(0x087044).into();
            theme.code_type = rgb(0x1D647C).into();
            theme.code_function = rgb(0x795E26).into();
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
            theme.code_keyword = rgb(0xB5A0E6).into();
            theme.code_literal = rgb(0xD9B872).into();
            theme.code_string = rgb(0x8EC8A6).into();
            theme.code_number = rgb(0xD9B872).into();
            theme.code_type = rgb(0x98BDEB).into();
            theme.code_function = rgb(0xBED0F3).into();
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
            theme.code_keyword = rgb(0x6C3EA1).into();
            theme.code_literal = rgb(0x844E0B).into();
            theme.code_string = rgb(0x246A48).into();
            theme.code_number = rgb(0x844E0B).into();
            theme.code_type = rgb(0x245D90).into();
            theme.code_function = rgb(0x315E8C).into();
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
            theme.code_keyword = rgb(0xB48EAD).into();
            theme.code_literal = rgb(0xB48EAD).into();
            theme.code_string = rgb(0xA3BE8C).into();
            theme.code_number = rgb(0xB48EAD).into();
            theme.code_type = rgb(0x8FBCBB).into();
            theme.code_function = rgb(0x88C0D0).into();
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
            theme.code_keyword = rgb(0x70446A).into();
            theme.code_literal = rgb(0x70446A).into();
            theme.code_string = rgb(0x46652E).into();
            theme.code_number = rgb(0x70446A).into();
            theme.code_type = rgb(0x285F5E).into();
            theme.code_function = rgb(0x385C89).into();
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

    pub(super) fn linear(is_dark: bool) -> Self {
        // Linear-inspired chrome with brand indigo #5E6AD2; the brighter
        // dark-mode fill uses dark ink to keep small button labels legible.
        let seed = if is_dark {
            PaletteSeed {
                canvas: Some(0x0F1011),
                sidebar: Some(0x0B0C0D),
                raised: Some(0x181A1B),
                composer: Some(0x1E2023),
                inset: Some(0x08090A),
                terminal: Some(0x0A0B0C),
                border: Some(0x26282B),
                border_strong: Some(0x35383C),
                text: Some(0xF7F8F8),
                text_secondary: Some(0xB0B4BB),
                text_tertiary: Some(0x8A8F98),
                text_muted: Some(0x7C828A),
                text_ghost: Some(0x7C828A),
                accent: Some(0x5E6AD2),
                accent_text: Some(0xA2ABF0),
                accent_fill: Some(0xA2ABF0),
                accent_focus: Some(0x7E89E8),
                on_accent: Some(0x1A1C1E),
                link: Some(0xA2ABF0),
                warning: Some(0xF2C94C),
                warning_text: Some(0xF2C94C),
                success: Some(0x4CB782),
                success_text: Some(0x4CB782),
                favorite: Some(0xF2C94C),
                danger: Some(0xEB5757),
                danger_text: Some(0xF07B7B),
                code_text: Some(0xE0C08C),
                code_keyword: Some(0xC39AF0),
                code_literal: Some(0xE8C57A),
                code_string: Some(0x7FC8A0),
                code_number: Some(0xE8C57A),
                code_type: Some(0x8FB8F0),
                code_function: Some(0x8FB8F0),
                ..PaletteSeed::default()
            }
        } else {
            PaletteSeed {
                canvas: Some(0xFFFFFF),
                sidebar: Some(0xF7F8F8),
                raised: Some(0xF2F3F5),
                composer: Some(0xFFFFFF),
                inset: Some(0xEDEEF1),
                terminal: Some(0xFFFFFF),
                border: Some(0xE4E6EA),
                border_strong: Some(0xC9CDD4),
                text: Some(0x1A1C1E),
                text_secondary: Some(0x5A5F66),
                text_tertiary: Some(0x595F66),
                text_muted: Some(0x595F66),
                text_ghost: Some(0x595F66),
                accent: Some(0x5E6AD2),
                accent_text: Some(0x4A55C4),
                accent_fill: Some(0x5E6AD2),
                accent_focus: Some(0x5E6AD2),
                on_accent: Some(0xFFFFFF),
                link: Some(0x4A55C4),
                warning: Some(0xA6690B),
                warning_text: Some(0x8A5A00),
                success: Some(0x2E8F5B),
                success_text: Some(0x236B43),
                favorite: Some(0xB08400),
                danger: Some(0xC2453F),
                danger_text: Some(0xB23A36),
                code_text: Some(0x95591C),
                code_keyword: Some(0x7A4FC9),
                code_literal: Some(0x795106),
                code_string: Some(0x246846),
                code_number: Some(0x795106),
                code_type: Some(0x2C5FA8),
                code_function: Some(0x2C5FA8),
                ..PaletteSeed::default()
            }
        };
        Self::family(ThemeScheme::Linear, is_dark, seed)
    }

    pub(super) fn notion(is_dark: bool) -> Self {
        // Notion's warm-neutral paper and ink. The brand blue #2383E2 carries
        // accent text and links in both appearances; the light fill is a shade
        // deeper so white labels clear 4.5:1, and the dark fill pairs with a
        // deep ink label for the same reason.
        let seed = if is_dark {
            PaletteSeed {
                canvas: Some(0x191919),
                sidebar: Some(0x151515),
                raised: Some(0x202020),
                composer: Some(0x252525),
                inset: Some(0x101010),
                terminal: Some(0x121212),
                border: Some(0x2E2E2E),
                border_strong: Some(0x3D3D3D),
                text: Some(0xEBEBE9),
                text_secondary: Some(0xA6A49F),
                text_tertiary: Some(0x8A8884),
                text_muted: Some(0xA6A49F),
                text_ghost: Some(0xA6A49F),
                accent: Some(0x2383E2),
                accent_text: Some(0x7AB8F0),
                accent_fill: Some(0x2383E2),
                accent_focus: Some(0x2383E2),
                on_accent: Some(0x0A1526),
                link: Some(0x7AB8F0),
                warning: Some(0xE9C46A),
                warning_text: Some(0xE9C46A),
                success: Some(0x5FB37C),
                success_text: Some(0x5FB37C),
                favorite: Some(0xE9C46A),
                danger: Some(0xEB6A5E),
                danger_text: Some(0xF58A80),
                code_text: Some(0xDFC08A),
                code_keyword: Some(0xC395E8),
                code_literal: Some(0xE0B457),
                code_string: Some(0x7CC49B),
                code_number: Some(0xE0B457),
                code_type: Some(0x8FA9E0),
                code_function: Some(0x8FA9E0),
                ..PaletteSeed::default()
            }
        } else {
            PaletteSeed {
                canvas: Some(0xFFFFFF),
                sidebar: Some(0xF7F6F3),
                raised: Some(0xF1F0ED),
                composer: Some(0xFFFFFF),
                inset: Some(0xEDECE8),
                terminal: Some(0xFFFFFF),
                border: Some(0xE9E7E2),
                border_strong: Some(0xD3D0CA),
                text: Some(0x37352F),
                text_secondary: Some(0x6B6A63),
                text_tertiary: Some(0x5D5B55),
                text_muted: Some(0x5D5B55),
                text_ghost: Some(0x5D5B55),
                accent: Some(0x2383E2),
                accent_text: Some(0x175FAF),
                accent_fill: Some(0x1B72CC),
                accent_focus: Some(0x2383E2),
                on_accent: Some(0xFFFFFF),
                link: Some(0x1B72CC),
                warning: Some(0xAD7A15),
                warning_text: Some(0x8A6114),
                success: Some(0x2E8F52),
                success_text: Some(0x236B3C),
                favorite: Some(0xA67C00),
                danger: Some(0xC2453F),
                danger_text: Some(0xB23A36),
                code_text: Some(0x8A6114),
                code_keyword: Some(0x7B3FA8),
                code_literal: Some(0x79531E),
                code_string: Some(0x246844),
                code_number: Some(0x79531E),
                code_type: Some(0x3A5BA8),
                code_function: Some(0x3A5BA8),
                ..PaletteSeed::default()
            }
        };
        Self::family(ThemeScheme::Notion, is_dark, seed)
    }

    pub(super) fn one(is_dark: bool) -> Self {
        // The One (Atom) theme's One Dark / One Light palettes: syntax colors
        // are the canonical blue #61AFEF, purple #C678DD, green #98C379,
        // gold #E5C07B family. Fills are those same hues darkened for the
        // light appearance so white labels clear 4.5:1.
        let seed = if is_dark {
            PaletteSeed {
                canvas: Some(0x282C34),
                sidebar: Some(0x21252B),
                raised: Some(0x2F343D),
                composer: Some(0x333944),
                inset: Some(0x1F2329),
                terminal: Some(0x1F2329),
                border: Some(0x3A4049),
                border_strong: Some(0x4B5262),
                text: Some(0xD7DCE4),
                text_secondary: Some(0x9BA1AC),
                text_tertiary: Some(0xA6ACB7),
                text_muted: Some(0x9BA1AC),
                text_ghost: Some(0x9BA1AC),
                accent: Some(0x61AFEF),
                accent_text: Some(0x61AFEF),
                accent_fill: Some(0x61AFEF),
                accent_focus: Some(0x61AFEF),
                on_accent: Some(0x21252B),
                link: Some(0x61AFEF),
                warning: Some(0xE5C07B),
                warning_text: Some(0xE5C07B),
                success: Some(0x98C379),
                success_text: Some(0x98C379),
                favorite: Some(0xE5C07B),
                danger: Some(0xE06C75),
                danger_text: Some(0xE89096),
                code_text: Some(0xE5C07B),
                code_keyword: Some(0xC678DD),
                code_literal: Some(0xD19A66),
                code_string: Some(0x98C379),
                code_number: Some(0xD19A66),
                code_type: Some(0xE5C07B),
                code_function: Some(0x61AFEF),
                ..PaletteSeed::default()
            }
        } else {
            PaletteSeed {
                canvas: Some(0xFFFFFF),
                sidebar: Some(0xF7F7F7),
                raised: Some(0xF3F3F3),
                composer: Some(0xFFFFFF),
                inset: Some(0xEEEEEE),
                terminal: Some(0xFFFFFF),
                border: Some(0xE4E6EB),
                border_strong: Some(0xC9CDD6),
                text: Some(0x383A42),
                text_secondary: Some(0x5A5E68),
                text_tertiary: Some(0x5A5E68),
                text_muted: Some(0x666A73),
                text_ghost: Some(0x666A73),
                accent: Some(0x4078F2),
                accent_text: Some(0x2F62C9),
                accent_fill: Some(0x3568D0),
                accent_focus: Some(0x4078F2),
                on_accent: Some(0xFFFFFF),
                link: Some(0x2F62C9),
                warning: Some(0xA6690B),
                warning_text: Some(0x8A5A00),
                success: Some(0x2E7D32),
                success_text: Some(0x246B2A),
                favorite: Some(0xA67C00),
                danger: Some(0xC2453F),
                danger_text: Some(0xB23A36),
                code_text: Some(0x94612A),
                code_keyword: Some(0xA626A4),
                code_literal: Some(0x8E6100),
                code_string: Some(0x356B2D),
                code_number: Some(0x8E6100),
                code_type: Some(0x795604),
                code_function: Some(0x2F62C9),
                ..PaletteSeed::default()
            }
        };
        Self::family(ThemeScheme::One, is_dark, seed)
    }

    pub(super) fn proof(is_dark: bool) -> Self {
        // Proof is adapted here rather than ported: there is no published
        // palette to match, so the family is built from a manuscript /
        // proofreading idea — warm paper and ink, a proofreader's amber for
        // interaction and annotation. Dark = warm charcoal with amber on a
        // deep ink label; light = warm paper with the amber deepened so white
        // labels clear 4.5:1.
        let seed = if is_dark {
            PaletteSeed {
                canvas: Some(0x1A1917),
                sidebar: Some(0x151412),
                raised: Some(0x232120),
                composer: Some(0x272423),
                inset: Some(0x100F0E),
                terminal: Some(0x100F0E),
                border: Some(0x322F2A),
                border_strong: Some(0x443F38),
                text: Some(0xEFEDE7),
                text_secondary: Some(0xBDB8AD),
                text_tertiary: Some(0x9C968A),
                text_muted: Some(0x8F897E),
                text_ghost: Some(0x8F897E),
                accent: Some(0xE0A458),
                accent_text: Some(0xE8B878),
                accent_fill: Some(0xE0A458),
                accent_focus: Some(0xE8B878),
                on_accent: Some(0x241A0E),
                link: Some(0xE8B878),
                warning: Some(0xD9B25C),
                warning_text: Some(0xD9B25C),
                success: Some(0x7EA95C),
                success_text: Some(0x7EA95C),
                favorite: Some(0xD9B25C),
                danger: Some(0xDB6B52),
                danger_text: Some(0xE5836C),
                code_text: Some(0xDEA86E),
                code_keyword: Some(0xB88BC8),
                code_literal: Some(0xDCA45E),
                code_string: Some(0x9CBF7E),
                code_number: Some(0xDCA45E),
                code_type: Some(0x86AED0),
                code_function: Some(0x86AED0),
                ..PaletteSeed::default()
            }
        } else {
            PaletteSeed {
                canvas: Some(0xFFFDF9),
                sidebar: Some(0xF7F1E8),
                raised: Some(0xF2EADC),
                composer: Some(0xFFFDF9),
                inset: Some(0xEFE7DA),
                terminal: Some(0xFFFDF9),
                border: Some(0xE7DFD1),
                border_strong: Some(0xD2C7B4),
                text: Some(0x2B2622),
                text_secondary: Some(0x5E564C),
                text_tertiary: Some(0x6F6659),
                text_muted: Some(0x6A6255),
                text_ghost: Some(0x6A6255),
                accent: Some(0xA66A1F),
                accent_text: Some(0x8F5A15),
                accent_fill: Some(0x8F5A15),
                accent_focus: Some(0xA66A1F),
                on_accent: Some(0xFFFFFF),
                link: Some(0x8F5A15),
                warning: Some(0x8A6114),
                warning_text: Some(0x7A5410),
                success: Some(0x2F7D4E),
                success_text: Some(0x236B40),
                favorite: Some(0x9C7500),
                danger: Some(0xB23A36),
                danger_text: Some(0x9E3330),
                code_text: Some(0x8A6114),
                code_keyword: Some(0x7A4B92),
                code_literal: Some(0x795106),
                code_string: Some(0x40703A),
                code_number: Some(0x795106),
                code_type: Some(0x345E8C),
                code_function: Some(0x345E8C),
                ..PaletteSeed::default()
            }
        };
        Self::family(ThemeScheme::Proof, is_dark, seed)
    }

    pub(super) fn raycast(is_dark: bool) -> Self {
        // Raycast's crimson #FF6363 over its near-black chrome. The dark fill
        // keeps the brand red and pairs it with a deep ink label; the light
        // appearance deepens the red one step for white labels.
        let seed = if is_dark {
            PaletteSeed {
                canvas: Some(0x191919),
                sidebar: Some(0x131314),
                raised: Some(0x222223),
                composer: Some(0x262628),
                inset: Some(0x0E0E0F),
                terminal: Some(0x0E0E0F),
                border: Some(0x2C2C2E),
                border_strong: Some(0x3A3A3C),
                text: Some(0xF5F5F5),
                text_secondary: Some(0xB4B4B6),
                text_tertiary: Some(0x929295),
                text_muted: Some(0x9C9C9F),
                text_ghost: Some(0x9C9C9F),
                accent: Some(0xFF6363),
                accent_text: Some(0xFF8A8A),
                accent_fill: Some(0xFF6363),
                accent_focus: Some(0xFF6363),
                on_accent: Some(0x2A0E0E),
                link: Some(0xFF8A8A),
                warning: Some(0xF0B429),
                warning_text: Some(0xF0B429),
                success: Some(0x4CC38A),
                success_text: Some(0x4CC38A),
                favorite: Some(0xF0B429),
                danger: Some(0xE5484D),
                danger_text: Some(0xF2777A),
                code_text: Some(0xE59C86),
                code_keyword: Some(0xC792EA),
                code_literal: Some(0xF2C14E),
                code_string: Some(0x8FD694),
                code_number: Some(0xF2C14E),
                code_type: Some(0x6FD3E0),
                code_function: Some(0x7FB4F5),
                ..PaletteSeed::default()
            }
        } else {
            PaletteSeed {
                canvas: Some(0xFFFFFF),
                sidebar: Some(0xF6F6F6),
                raised: Some(0xF1F1F1),
                composer: Some(0xFFFFFF),
                inset: Some(0xEAEAEA),
                terminal: Some(0xFFFFFF),
                border: Some(0xE7E7E7),
                border_strong: Some(0xD4D4D4),
                text: Some(0x1F1F1F),
                text_secondary: Some(0x555557),
                text_tertiary: Some(0x6B6B6E),
                text_muted: Some(0x66666A),
                text_ghost: Some(0x66666A),
                accent: Some(0xE5484D),
                accent_text: Some(0xC63638),
                accent_fill: Some(0xCE3A3F),
                accent_focus: Some(0xE5484D),
                on_accent: Some(0xFFFFFF),
                link: Some(0xC63638),
                warning: Some(0xA6690B),
                warning_text: Some(0x8A5A00),
                success: Some(0x2E8F5B),
                success_text: Some(0x236B43),
                favorite: Some(0xA67C00),
                danger: Some(0xB23A36),
                danger_text: Some(0x9E3330),
                code_text: Some(0xB03A36),
                code_keyword: Some(0x9B3FB0),
                code_literal: Some(0x795106),
                code_string: Some(0x246846),
                code_number: Some(0x795106),
                code_type: Some(0x2A6E8C),
                code_function: Some(0x2C5FA8),
                ..PaletteSeed::default()
            }
        };
        Self::family(ThemeScheme::Raycast, is_dark, seed)
    }

    pub(super) fn rose_pine(is_dark: bool) -> Self {
        // Rose Pine's own Main (dark) and Dawn (light) palettes, using its
        // canonical roles: base/surface/overlay as chrome, iris as accent,
        // foam for links and success, gold for warning, love for danger.
        // A few paper-side syntax hues are a shade deeper than the released
        // values to stay legible on the warm light background.
        let seed = if is_dark {
            PaletteSeed {
                canvas: Some(0x191724),
                sidebar: Some(0x1F1D2E),
                raised: Some(0x26233A),
                composer: Some(0x26233A),
                inset: Some(0x151320),
                terminal: Some(0x13111C),
                border: Some(0x403D52),
                border_strong: Some(0x524F67),
                text: Some(0xE0DEF4),
                text_secondary: Some(0x908CAA),
                text_tertiary: Some(0x908CAA),
                text_muted: Some(0xAAA6C1),
                text_ghost: Some(0xAAA6C1),
                accent: Some(0xC4A7E7),
                accent_text: Some(0xC4A7E7),
                accent_fill: Some(0xC4A7E7),
                accent_focus: Some(0xC4A7E7),
                on_accent: Some(0x191724),
                link: Some(0x9CCFD8),
                warning: Some(0xF6C177),
                warning_text: Some(0xF6C177),
                success: Some(0x9CCFD8),
                success_text: Some(0x9CCFD8),
                favorite: Some(0xF6C177),
                danger: Some(0xEB6F92),
                danger_text: Some(0xEB6F92),
                code_text: Some(0xEBBCBA),
                code_keyword: Some(0xC4A7E7),
                code_literal: Some(0xF6C177),
                code_string: Some(0x56919F),
                code_number: Some(0xF6C177),
                code_type: Some(0x9CCFD8),
                code_function: Some(0xEBBCBA),
                ..PaletteSeed::default()
            }
        } else {
            PaletteSeed {
                canvas: Some(0xFAF4ED),
                sidebar: Some(0xF7F0E6),
                raised: Some(0xFFFAF3),
                composer: Some(0xFFFAF3),
                inset: Some(0xF2E9DE),
                terminal: Some(0xFFFAF3),
                border: Some(0xEBDFCF),
                border_strong: Some(0xD9C7B4),
                text: Some(0x575279),
                text_secondary: Some(0x585475),
                text_tertiary: Some(0x585475),
                text_muted: Some(0x585475),
                text_ghost: Some(0x585475),
                accent: Some(0x907AA9),
                accent_text: Some(0x6F5A8C),
                accent_fill: Some(0x7C6699),
                accent_focus: Some(0x907AA9),
                on_accent: Some(0xFFFFFF),
                link: Some(0x6F5A8C),
                warning: Some(0xB97B22),
                warning_text: Some(0x8A5E12),
                success: Some(0x286983),
                success_text: Some(0x1F566B),
                favorite: Some(0xB97B22),
                danger: Some(0xB4637A),
                danger_text: Some(0x9E4F66),
                code_text: Some(0x9E4F66),
                code_keyword: Some(0x69507D),
                code_literal: Some(0x8A5E12),
                code_string: Some(0x286983),
                code_number: Some(0x8A5E12),
                code_type: Some(0x285D6A),
                code_function: Some(0x884259),
                ..PaletteSeed::default()
            }
        };
        Self::family(ThemeScheme::RosePine, is_dark, seed)
    }

    pub(super) fn solarized(is_dark: bool) -> Self {
        // Solarized's published palette with its standard role swap: the dark
        // appearance builds from base03/base02, the light from base3/base2.
        // Accent hues stay official (blue, cyan, yellow, green); the text ramp
        // and inline code are adapted for legibility at transcript sizes.
        let seed = if is_dark {
            PaletteSeed {
                canvas: Some(0x002B36),
                sidebar: Some(0x073642),
                raised: Some(0x073642),
                composer: Some(0x0A4152),
                inset: Some(0x001F26),
                terminal: Some(0x002B36),
                border: Some(0x0C3E4B),
                border_strong: Some(0x16505E),
                text: Some(0xB9C6C6),
                text_secondary: Some(0xA8B7B8),
                text_tertiary: Some(0x9BACAD),
                text_muted: Some(0x9BACAD),
                text_ghost: Some(0x9BACAD),
                accent: Some(0x268BD2),
                accent_text: Some(0x4FA8D8),
                accent_fill: Some(0x268BD2),
                accent_focus: Some(0x268BD2),
                on_accent: Some(0x042027),
                link: Some(0x4FA8D8),
                warning: Some(0xC79A1E),
                warning_text: Some(0xC79A1E),
                success: Some(0xA0B016),
                success_text: Some(0xA0B016),
                favorite: Some(0xB58900),
                danger: Some(0xDC322F),
                danger_text: Some(0xF0706D),
                code_text: Some(0xD9762F),
                code_keyword: Some(0x9A93E8),
                code_literal: Some(0xC79A1E),
                code_string: Some(0x2AA198),
                code_number: Some(0xC79A1E),
                code_type: Some(0x268BD2),
                code_function: Some(0x859900),
                ..PaletteSeed::default()
            }
        } else {
            PaletteSeed {
                canvas: Some(0xFDF6E3),
                sidebar: Some(0xF6EEDF),
                raised: Some(0xFBF4E3),
                composer: Some(0xFFF9EC),
                inset: Some(0xEEE8D5),
                terminal: Some(0xFDF6E3),
                border: Some(0xE8E0CB),
                border_strong: Some(0xD6CBAF),
                text: Some(0x3F565E),
                text_secondary: Some(0x496169),
                text_tertiary: Some(0x4D6269),
                text_muted: Some(0x496169),
                text_ghost: Some(0x496169),
                accent: Some(0x268BD2),
                accent_text: Some(0x1F6E9E),
                accent_fill: Some(0x1F6E9E),
                accent_focus: Some(0x2A7FB8),
                on_accent: Some(0xFFFFFF),
                link: Some(0x1F6E9E),
                warning: Some(0x8A6A00),
                warning_text: Some(0x7A5D00),
                success: Some(0x5F6E00),
                success_text: Some(0x526008),
                favorite: Some(0x8A6A00),
                danger: Some(0xC43B39),
                danger_text: Some(0xA82A28),
                code_text: Some(0xA33C14),
                code_keyword: Some(0x4D55A6),
                code_literal: Some(0x705400),
                code_string: Some(0x0E756B),
                code_number: Some(0x705400),
                code_type: Some(0x1F6E9E),
                code_function: Some(0x526008),
                ..PaletteSeed::default()
            }
        };
        Self::family(ThemeScheme::Solarized, is_dark, seed)
    }

    pub(super) fn vercel(is_dark: bool) -> Self {
        // Geist-inspired black/white chrome and blue #0070F3 accents; on dark
        // surfaces controls use a brighter blue fill with dark ink labels.
        let seed = if is_dark {
            PaletteSeed {
                canvas: Some(0x0A0A0A),
                sidebar: Some(0x000000),
                raised: Some(0x171717),
                composer: Some(0x1A1A1A),
                inset: Some(0x000000),
                terminal: Some(0x000000),
                border: Some(0x2A2A2A),
                border_strong: Some(0x3A3A3A),
                text: Some(0xEDEDED),
                text_secondary: Some(0xA1A1A1),
                text_tertiary: Some(0x8F8F8F),
                text_muted: Some(0x828282),
                text_ghost: Some(0x828282),
                accent: Some(0x0070F3),
                accent_text: Some(0x3B9AF7),
                accent_fill: Some(0x3B9AF7),
                accent_focus: Some(0x0070F3),
                on_accent: Some(0x171717),
                link: Some(0x3B9AF7),
                warning: Some(0xF5A623),
                warning_text: Some(0xF5A623),
                success: Some(0x3ECF8E),
                success_text: Some(0x3ECF8E),
                favorite: Some(0xF5A623),
                danger: Some(0xF2555A),
                danger_text: Some(0xF2777A),
                code_text: Some(0x5AA2F0),
                code_keyword: Some(0x8B7CF6),
                code_literal: Some(0xF5A623),
                code_string: Some(0x3ECF8E),
                code_number: Some(0xF5A623),
                code_type: Some(0x3B9AF7),
                code_function: Some(0x3B9AF7),
                ..PaletteSeed::default()
            }
        } else {
            PaletteSeed {
                canvas: Some(0xFFFFFF),
                sidebar: Some(0xFAFAFA),
                raised: Some(0xF5F5F5),
                composer: Some(0xFFFFFF),
                inset: Some(0xF0F0F0),
                terminal: Some(0xFFFFFF),
                border: Some(0xE5E5E5),
                border_strong: Some(0xD4D4D4),
                text: Some(0x171717),
                text_secondary: Some(0x444444),
                text_tertiary: Some(0x5C5C5C),
                text_muted: Some(0x575757),
                text_ghost: Some(0x575757),
                accent: Some(0x0070F3),
                accent_text: Some(0x0761D1),
                accent_fill: Some(0x0070F3),
                accent_focus: Some(0x0070F3),
                on_accent: Some(0xFFFFFF),
                link: Some(0x0761D1),
                warning: Some(0xA16207),
                warning_text: Some(0x8A5A00),
                success: Some(0x0F7B52),
                success_text: Some(0x0B6342),
                favorite: Some(0xA16207),
                danger: Some(0xC22A34),
                danger_text: Some(0xAB2430),
                code_text: Some(0x0B5FC0),
                code_keyword: Some(0x6B3FC9),
                code_literal: Some(0x8A5A00),
                code_string: Some(0x0F7B52),
                code_number: Some(0x8A5A00),
                code_type: Some(0x2A5FA8),
                code_function: Some(0x2A5FA8),
                ..PaletteSeed::default()
            }
        };
        Self::family(ThemeScheme::Vercel, is_dark, seed)
    }

    pub(super) fn vs_code_plus(is_dark: bool) -> Self {
        // VS Code Plus pairs the shipped Light+ / Dark+ workbench chrome with
        // the Light+ / Dark+ token colors from the VS Code theme reference:
        // classic #252526 sidebar on dark, #F3F3F3 on light, and the official
        // purple/classic-blue/pine/green/gold syntax set. Small labels over
        // the bright dark-mode blue use ink rather than white.
        let seed = if is_dark {
            PaletteSeed {
                canvas: Some(0x1F1F1F),
                sidebar: Some(0x252526),
                raised: Some(0x2D2D2D),
                composer: Some(0x333333),
                inset: Some(0x181818),
                terminal: Some(0x1F1F1F),
                border: Some(0x2B2B2B),
                border_strong: Some(0x434343),
                text: Some(0xD4D4D4),
                text_secondary: Some(0xBDBDBD),
                text_tertiary: Some(0xAAAAAA),
                text_muted: Some(0x9B9B9B),
                text_ghost: Some(0x9B9B9B),
                accent: Some(0x0098F7),
                accent_text: Some(0x4CB2F5),
                accent_fill: Some(0x4CB2F5),
                accent_focus: Some(0x0098F7),
                on_accent: Some(0x1F1F1F),
                link: Some(0x3794FF),
                warning: Some(0xCCA700),
                warning_text: Some(0xCCA700),
                success: Some(0x89D185),
                success_text: Some(0x89D185),
                favorite: Some(0xD7BA7D),
                danger: Some(0xF14C4C),
                danger_text: Some(0xF48771),
                code_text: Some(0xCE9178),
                code_keyword: Some(0xC586C0),
                code_literal: Some(0xB5CEA8),
                code_string: Some(0xCE9178),
                code_number: Some(0xB5CEA8),
                code_type: Some(0x4EC9B0),
                code_function: Some(0xDCDCAA),
                ..PaletteSeed::default()
            }
        } else {
            PaletteSeed {
                canvas: Some(0xFFFFFF),
                sidebar: Some(0xF3F3F3),
                raised: Some(0xF8F8F8),
                composer: Some(0xFFFFFF),
                inset: Some(0xF3F3F3),
                terminal: Some(0xFFFFFF),
                border: Some(0xE5E5E5),
                border_strong: Some(0xC8C8C8),
                text: Some(0x3B3B3B),
                text_secondary: Some(0x595959),
                text_tertiary: Some(0x6B6B6B),
                text_muted: Some(0x656565),
                text_ghost: Some(0x656565),
                accent: Some(0x005FB8),
                accent_text: Some(0x005FB8),
                accent_fill: Some(0x005FB8),
                accent_focus: Some(0x0090F1),
                on_accent: Some(0xFFFFFF),
                link: Some(0x005FB8),
                warning: Some(0x8A6A00),
                warning_text: Some(0x7A5D00),
                success: Some(0x2E7D32),
                success_text: Some(0x246B2A),
                favorite: Some(0x8A6A00),
                danger: Some(0xC4314B),
                danger_text: Some(0xAB2440),
                code_text: Some(0xA31515),
                code_keyword: Some(0xAF00DB),
                code_literal: Some(0x087044),
                code_string: Some(0xA31515),
                code_number: Some(0x087044),
                code_type: Some(0x1D647C),
                code_function: Some(0x795E26),
                ..PaletteSeed::default()
            }
        };
        Self::family(ThemeScheme::VsCodePlus, is_dark, seed)
    }
}
