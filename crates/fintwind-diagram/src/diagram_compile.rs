//! Compiles Mermaid source into portable SVG for the markdown reader.
//!
//! Ondine (crate `ondine`, package `compiler`) emits themed SVG with CSS custom properties and
//! `color-mix()`. usvg supports neither, so this module resolves them against a
//! small [`Theme`] and removes the now-unused custom-property declarations. The
//! resolver never touches text nodes: a label that says `var(--x)` stays literal.
//!
//! This module lives in `fintwind-core` so the daemon's `mermaid` check tool and
//! the app's renderer call one identical compiler; a validation can never drift
//! from what the conversation actually draws.
//!
//! See `docs/diagrams.md` for the failure list, theme mapping and acceptance cases.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

/// The five colors a diagram is drawn with, as sRGB channels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Theme {
    /// The page and node surface color.
    pub background: [u8; 3],
    /// The main text, line and default shape color.
    pub foreground: [u8; 3],
    /// Secondary text and connector lines.
    pub muted: [u8; 3],
    /// Shape outlines.
    pub border: [u8; 3],
    /// Emphasis: accent series, critical marks, focus.
    pub accent: [u8; 3],
}

/// A rendered diagram, sized in CSS pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagram {
    /// A self-contained SVG document.
    pub svg: String,
    /// The natural width in pixels.
    pub width: f64,
    /// The natural height in pixels.
    pub height: f64,
}

/// The most source bytes accepted.
pub const MAX_SOURCE_BYTES: usize = 32 * 1024;
/// The most SVG bytes produced.
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// The most edges a flowchart may describe.
const MAX_EDGES: u32 = 512;
/// The largest natural width or height in pixels.
const MAX_DIMENSION: f64 = 100_000.0;
/// A cycle in local custom properties stops here.
const MAX_RESOLVE_DEPTH: usize = 16;
/// A pathological nesting of `color-mix()` stops here.
const MAX_MIX_ROUNDS: usize = 64;
/// The root family. Ondine measured with a monospace advance; this font has it.
const ROOT_FONT: &str = "JetBrains Mono, monospace";

/// Attributes that can carry a drawing value. Text nodes are never inspected.
const DRAWING_ATTRIBUTES: [&str; 14] = [
    "fill",
    "stroke",
    "color",
    "stop-color",
    "flood-color",
    "stroke-width",
    "font-family",
    "font-weight",
    "font-size",
    "rx",
    "ry",
    "marker-start",
    "marker-mid",
    "marker-end",
];

/// The monochrome shades behind series 2..=8, as foreground percentages over
/// the background; series 1 is the theme accent. The order alternates light
/// and dark so neighboring slices never blur together.
const SERIES_SHADES: [u8; 7] = [57, 86, 50, 79, 64, 93, 71];

/// Compile against a captured reference date, or a deterministic acceptance date.
///
/// # Errors
/// Returns an error for invalid source, exceeded limits, an unusable size, or
/// SVG that cannot be made safe and portable.
pub fn compile(source: &str, theme: Theme, now_ms: f64) -> Result<Diagram> {
    anyhow::ensure!(now_ms.is_finite(), "invalid diagram reference date");
    if source.trim().is_empty() {
        bail!("diagram source is empty");
    }
    if source.len() > MAX_SOURCE_BYTES {
        bail!(
            "diagram source is {} bytes; the limit is {MAX_SOURCE_BYTES} bytes",
            source.len()
        );
    }

    let options = format!(
        "{{\"limit\":{MAX_SOURCE_BYTES},\"outputLimit\":{MAX_OUTPUT_BYTES},\"maxEdges\":{MAX_EDGES},\"images\":false,\"links\":false,\"responsive\":false}}"
    );
    let rendered = ondine::render(source, &options, now_ms).map_err(|error| {
        let location = match (error.line, error.column) {
            (0, _) => String::new(),
            (line, 0) => format!(" at line {line}"),
            (line, column) => format!(" at {line}:{column}"),
        };
        anyhow::anyhow!(
            "diagram {} error{location}: {}",
            error.code.as_str(),
            error.message
        )
    })?;

    if !rendered.width.is_finite()
        || !rendered.height.is_finite()
        || rendered.width <= 0.0
        || rendered.height <= 0.0
        || rendered.width > MAX_DIMENSION
        || rendered.height > MAX_DIMENSION
    {
        bail!(
            "diagram size {} x {} is outside the allowed range",
            rendered.width,
            rendered.height
        );
    }

    let svg = materialize(&rendered.svg, theme).context("materializing diagram theme")?;
    if svg.len() > MAX_OUTPUT_BYTES {
        bail!(
            "diagram SVG is {} bytes; the limit is {MAX_OUTPUT_BYTES} bytes",
            svg.len()
        );
    }
    audit(&svg).context("auditing diagram SVG")?;

    Ok(Diagram {
        svg,
        width: rendered.width,
        height: rendered.height,
    })
}

/// Replaces CSS variables and `color-mix()` in start-tag attributes.
fn materialize(svg: &str, theme: Theme) -> Result<String> {
    let vars = theme_vars(theme);
    let mut out = String::with_capacity(svg.len());
    let mut at = 0;
    while at < svg.len() {
        let rest = svg.get(at..).unwrap_or("");
        if !rest.starts_with('<') {
            let end = rest.find('<').map_or(svg.len(), |offset| at + offset);
            out.push_str(svg.get(at..end).unwrap_or(""));
            at = end;
            continue;
        }

        if rest.starts_with("<!--") {
            at = copy_through(svg, at, "-->", &mut out)?;
        } else if rest.starts_with("<![CDATA[") {
            at = copy_through(svg, at, "]]>", &mut out)?;
        } else if rest.starts_with("<?") {
            at = copy_through(svg, at, "?>", &mut out)?;
        } else if rest.starts_with("<!") || rest.starts_with("</") {
            at = copy_through(svg, at, ">", &mut out)?;
        } else {
            let end = tag_end(svg, at)?;
            let tag = svg.get(at..end).unwrap_or("");
            out.push_str(&rewrite_tag(tag, &vars)?);
            at = end;
        }
    }
    Ok(out)
}

/// Copies the text up to and including `needle`, and returns the new position.
fn copy_through(svg: &str, at: usize, needle: &str, out: &mut String) -> Result<usize> {
    let end = find_after(svg, at, needle)?;
    out.push_str(svg.get(at..end).unwrap_or(""));
    Ok(end)
}

/// Finds the end of `needle` from `at`, including the needle.
fn find_after(svg: &str, at: usize, needle: &str) -> Result<usize> {
    svg.get(at..)
        .and_then(|rest| rest.find(needle))
        .map(|offset| at + offset + needle.len())
        .with_context(|| format!("unterminated SVG markup at byte {at}"))
}

/// The index just past a start tag's `>`, ignoring `>` inside quotes.
fn tag_end(svg: &str, at: usize) -> Result<usize> {
    let bytes = svg.as_bytes();
    let mut index = at + 1;
    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(ending) => {
                if byte == ending {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'>' => return Ok(index + 1),
                _ => {}
            },
        }
        index += 1;
    }
    bail!("unterminated SVG tag at byte {at}")
}

/// One attribute inside a start tag, with byte ranges into the tag.
#[derive(Clone, Debug)]
struct AttrSpan {
    /// The byte just before the attribute's leading whitespace.
    start: usize,
    /// The byte just past the attribute's value.
    end: usize,
    /// The attribute name.
    name: std::ops::Range<usize>,
    /// The value between quotes, or the bare value.
    value: std::ops::Range<usize>,
    /// Whether the value was quoted.
    quoted: bool,
}

/// Reads the attributes of a start tag. The tag name is skipped.
fn tag_attributes(tag: &str) -> Vec<AttrSpan> {
    let bytes = tag.as_bytes();
    let mut spans = Vec::new();
    let mut index = 1;
    while index < bytes.len()
        && !bytes[index].is_ascii_whitespace()
        && !matches!(bytes[index], b'>' | b'/')
    {
        index += 1;
    }

    while index < bytes.len() {
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || matches!(bytes[index], b'>' | b'/') {
            break;
        }

        let name_start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && !matches!(bytes[index], b'=' | b'>' | b'/')
        {
            index += 1;
        }
        let name_end = index;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }

        let mut value = name_end..name_end;
        let mut quoted = false;
        if index < bytes.len() && bytes[index] == b'=' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index < bytes.len() && (bytes[index] == b'"' || bytes[index] == b'\'') {
                let quote = bytes[index];
                index += 1;
                let value_start = index;
                while index < bytes.len() && bytes[index] != quote {
                    index += 1;
                }
                value = value_start..index;
                quoted = true;
                if index < bytes.len() {
                    index += 1;
                }
            } else {
                let value_start = index;
                while index < bytes.len()
                    && !bytes[index].is_ascii_whitespace()
                    && !matches!(bytes[index], b'>' | b'/')
                {
                    index += 1;
                }
                value = value_start..index;
            }
        }

        spans.push(AttrSpan {
            start,
            end: index,
            name: name_start..name_end,
            value,
            quoted,
        });
    }
    spans
}

/// Rewrites only the attributes that carry theme variables or a color mix.
fn rewrite_tag(tag: &str, vars: &HashMap<String, String>) -> Result<String> {
    let spans = tag_attributes(tag);
    if spans.is_empty() {
        return Ok(tag.to_owned());
    }

    let mut out = String::with_capacity(tag.len());
    let mut cursor = 0;
    for span in &spans {
        out.push_str(tag.get(cursor..span.start).unwrap_or(""));
        let name = tag.get(span.name.clone()).unwrap_or("");
        let value = tag.get(span.value.clone()).unwrap_or("");
        if !span.quoted || !needs_materialization(name, value) {
            out.push_str(tag.get(span.start..span.end).unwrap_or(""));
        } else {
            let replaced = if name.eq_ignore_ascii_case("style") {
                rewrite_style(value, vars)
            } else {
                resolve_expr(value, vars)
            }
            .with_context(|| format!("resolving attribute {name}"))?;
            out.push_str(tag.get(span.start..span.name.start).unwrap_or(""));
            out.push_str(name);
            out.push_str("=\"");
            push_escaped(&mut out, &replaced);
            out.push('"');
        }
        cursor = span.end;
    }
    out.push_str(tag.get(cursor..).unwrap_or(""));
    Ok(out)
}

/// Whether an attribute is a drawing value that this module must resolve.
fn needs_materialization(name: &str, value: &str) -> bool {
    if name.eq_ignore_ascii_case("style") {
        return contains_ci(value, "var(")
            || contains_ci(value, "color-mix(")
            || value.contains("--");
    }
    DRAWING_ATTRIBUTES
        .iter()
        .any(|attribute| name.eq_ignore_ascii_case(attribute))
        && (contains_ci(value, "var(") || contains_ci(value, "color-mix("))
}

/// Resolves a `style` attribute and drops its private custom properties.
fn rewrite_style(style: &str, vars: &HashMap<String, String>) -> Result<String> {
    let declarations = split_top_level(style, ';');

    // Local custom properties can be referenced in any order, so collect them first.
    let mut local = HashMap::new();
    for declaration in &declarations {
        let Some(colon) = find_top_level(declaration, ':') else {
            continue;
        };
        let name = declaration
            .get(..colon)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let value = declaration.get(colon + 1..).unwrap_or("").trim();
        if name.starts_with("--") && !value.is_empty() {
            local.insert(name, value.to_owned());
        }
    }

    let mut scoped = vars.clone();
    scoped.extend(local);

    let mut out = String::new();
    for declaration in &declarations {
        let Some(colon) = find_top_level(declaration, ':') else {
            continue;
        };
        let name = declaration
            .get(..colon)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let value = declaration.get(colon + 1..).unwrap_or("").trim();
        if name.is_empty() || value.is_empty() || name.starts_with("--") {
            continue;
        }
        let value = if contains_ci(value, "var(") || contains_ci(value, "color-mix(") {
            resolve_expr(value, &scoped)?
        } else {
            value.to_owned()
        };
        if contains_ci(&value, "var(") || contains_ci(&value, "color-mix(") {
            bail!("declaration {name} still contains unresolved CSS");
        }
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(&name);
        out.push(':');
        out.push_str(&value);
    }
    Ok(out)
}

/// Replaces `var()` references from `vars`, using fallbacks where given.
fn resolve_vars(input: &str, vars: &HashMap<String, String>, depth: usize) -> Result<String> {
    if depth > MAX_RESOLVE_DEPTH {
        bail!("CSS variable nesting is deeper than {MAX_RESOLVE_DEPTH}");
    }
    if !contains_ci(input, "var(") {
        return Ok(input.to_owned());
    }

    let mut out = String::with_capacity(input.len());
    let mut at = 0;
    while let Some(found) = find_ci(input.get(at..).unwrap_or(""), "var(") {
        let start = at + found;
        out.push_str(input.get(at..start).unwrap_or(""));
        let open = start + 3;
        let close = matching_paren(input, open)?;
        let inner = input.get(open + 1..close).unwrap_or("");
        let (name, fallback) = split_first_comma(inner);
        let name = name.trim();
        if name.is_empty() {
            bail!("empty CSS variable name");
        }

        let value = if let Some(value) = vars.get(name) {
            value.clone()
        } else if let Some(fallback) = fallback {
            fallback.trim().to_owned()
        } else {
            // The output must not keep an unresolved variable. A missing fallback is an
            // author typo, not a reason to drop the whole diagram.
            vars.get("--ondine-fg")
                .cloned()
                .unwrap_or_else(|| "#000000".to_owned())
        };
        out.push_str(&resolve_vars(&value, vars, depth + 1)?);
        at = close + 1;
    }
    out.push_str(input.get(at..).unwrap_or(""));
    Ok(out)
}

/// Folds every `color-mix()`, innermost first.
fn resolve_mixes(input: &str, vars: &HashMap<String, String>) -> Result<String> {
    if !contains_ci(input, "color-mix(") {
        return Ok(input.to_owned());
    }

    let mut text = input.to_owned();
    for _ in 0..MAX_MIX_ROUNDS {
        let Some(start) = find_last_ci(&text, "color-mix(") else {
            return Ok(text);
        };
        let open = start + 9;
        let close = matching_paren(&text, open)?;
        let inner = text.get(open + 1..close).unwrap_or("");
        let mixed = mix_srgb(inner, vars)?;
        text.replace_range(start..=close, &mixed);
    }
    bail!("color-mix() nesting is deeper than {MAX_MIX_ROUNDS}")
}

/// Resolves variables, then color mixes, in one attribute value.
fn resolve_expr(input: &str, vars: &HashMap<String, String>) -> Result<String> {
    let substituted = resolve_vars(input, vars, 0)?;
    resolve_mixes(&substituted, vars)
}

/// Mixes two colors with CSS `color-mix(in srgb, ...)` semantics.
fn mix_srgb(inner: &str, vars: &HashMap<String, String>) -> Result<String> {
    let parts = split_top_level(inner, ',');
    if parts.len() != 2 {
        bail!("color-mix() needs exactly two colors");
    }
    let space = parts[0].trim();
    if !space.eq_ignore_ascii_case("in srgb") {
        bail!("unsupported color-mix() space: {space}");
    }

    let (first_text, first_percent) = color_argument(parts[1])?;
    let (second_text, second_percent) = color_argument(parts[2])?;
    let first = parse_color(first_text, vars)?;
    let second = parse_color(second_text, vars)?;

    let (first_weight, second_weight) = match (first_percent, second_percent) {
        (None, None) => (50.0, 50.0),
        (Some(percent), None) => (percent, 100.0 - percent),
        (None, Some(percent)) => (100.0 - percent, percent),
        (Some(first), Some(second)) => (first, second),
    };
    let total = first_weight + second_weight;
    let (first_share, second_share) = if total <= f64::EPSILON {
        (0.5, 0.5)
    } else {
        (first_weight / total, second_weight / total)
    };
    let alpha_multiplier = (total / 100.0).clamp(0.0, 1.0);

    let blend = |first: u8, second: u8| -> u8 {
        let value = f64::from(first) * first_share + f64::from(second) * second_share;
        channel(value)
    };
    let red = blend(first.red, second.red);
    let green = blend(first.green, second.green);
    let blue = blend(first.blue, second.blue);
    let alpha = (first.alpha * first_share + second.alpha * second_share) * alpha_multiplier;
    Ok(format_color(red, green, blue, alpha))
}

/// Splits a color argument into its color and an optional trailing percentage.
fn color_argument(part: &str) -> Result<(&str, Option<f64>)> {
    let text = part.trim();
    if let Some(index) = text.rfind(char::is_whitespace) {
        let tail = text.get(index..).unwrap_or("").trim();
        if let Some(number) = tail.strip_suffix('%') {
            let percent: f64 = number
                .trim()
                .parse()
                .with_context(|| format!("invalid color-mix() percentage: {tail}"))?;
            if !percent.is_finite() {
                bail!("color-mix() percentage is not finite");
            }
            return Ok((
                text.get(..index).unwrap_or("").trim(),
                Some(percent.clamp(0.0, 100.0)),
            ));
        }
    }
    Ok((text, None))
}

/// A color with straight (non-premultiplied) alpha in `0.0..=1.0`.
#[derive(Clone, Copy, Debug)]
struct Color {
    red: u8,
    green: u8,
    blue: u8,
    alpha: f64,
}

/// Parses a color that appears inside a `color-mix()`.
fn parse_color(text: &str, vars: &HashMap<String, String>) -> Result<Color> {
    let text = text.trim();
    if text.is_empty() {
        bail!("empty color in color-mix()");
    }
    if let Some(hex) = text.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Some(open) = text.find('(') {
        let prefix = text.get(..open).unwrap_or("").trim().to_ascii_lowercase();
        if prefix == "rgb" || prefix == "rgba" {
            return parse_rgb(text, open);
        }
        bail!("unsupported color function in color-mix(): {text}");
    }

    let lower = text.to_ascii_lowercase();
    if lower == "currentcolor" {
        let foreground = vars
            .get("--ondine-fg")
            .map(String::as_str)
            .unwrap_or("#000000");
        return parse_hex(foreground.strip_prefix('#').unwrap_or(foreground));
    }
    if lower == "transparent" {
        return Ok(Color {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0.0,
        });
    }
    if let Some(color) = named_color(&lower) {
        return Ok(color);
    }
    bail!("unsupported color in color-mix(): {text}")
}

/// Parses `#rgb`, `#rgba`, `#rrggbb` and `#rrggbbaa`.
fn parse_hex(text: &str) -> Result<Color> {
    let digits = text.trim();
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid hex color: #{digits}");
    }
    let normalized = match digits.len() {
        3 | 4 => digits
            .chars()
            .flat_map(|character| [character, character])
            .collect::<String>(),
        6 | 8 => digits.to_owned(),
        _ => bail!("invalid hex color length: #{digits}"),
    };
    let red = hex_channel(normalized.get(0..2).unwrap_or(""))?;
    let green = hex_channel(normalized.get(2..4).unwrap_or(""))?;
    let blue = hex_channel(normalized.get(4..6).unwrap_or(""))?;
    let alpha = if normalized.len() == 8 {
        f64::from(hex_channel(normalized.get(6..8).unwrap_or(""))?) / 255.0
    } else {
        1.0
    };
    Ok(Color {
        red,
        green,
        blue,
        alpha,
    })
}

/// One two-digit hex channel.
fn hex_channel(pair: &str) -> Result<u8> {
    u8::from_str_radix(pair, 16).with_context(|| format!("invalid hex color channel: {pair}"))
}

/// Parses `rgb()` and `rgba()` in comma or space syntax.
fn parse_rgb(text: &str, open: usize) -> Result<Color> {
    let Some(inner) = text.get(open + 1..text.len().saturating_sub(1)) else {
        bail!("unterminated rgb() color");
    };
    if !text.ends_with(')') {
        bail!("unterminated rgb() color");
    }

    let comma_parts = split_top_level(inner, ',');
    let (channels, alpha_text) = if comma_parts.len() >= 3 {
        let alpha = comma_parts.get(3).copied();
        (
            comma_parts.iter().take(3).copied().collect::<Vec<_>>(),
            alpha,
        )
    } else {
        let (body, alpha) = match inner.split_once('/') {
            Some((body, alpha)) => (body, Some(alpha)),
            None => (inner, None),
        };
        (body.split_ascii_whitespace().collect(), alpha)
    };
    if channels.len() != 3 {
        bail!("rgb() needs exactly three channels");
    }

    let red = rgb_channel(channels[0])?;
    let green = rgb_channel(channels[1])?;
    let blue = rgb_channel(channels[2])?;
    let alpha = match alpha_text {
        Some(alpha) => rgb_alpha(alpha)?,
        None => 1.0,
    };
    Ok(Color {
        red,
        green,
        blue,
        alpha,
    })
}

/// Parses an `rgb()` channel as a number or a percentage.
fn rgb_channel(text: &str) -> Result<u8> {
    let text = text.trim();
    if let Some(percent) = text.strip_suffix('%') {
        let value: f64 = percent
            .trim()
            .parse()
            .with_context(|| format!("invalid rgb() channel: {text}"))?;
        if !value.is_finite() {
            bail!("rgb() channel is not finite");
        }
        return Ok(channel(value.clamp(0.0, 100.0) * 255.0 / 100.0));
    }
    let value: f64 = text
        .parse()
        .with_context(|| format!("invalid rgb() channel: {text}"))?;
    if !value.is_finite() {
        bail!("rgb() channel is not finite");
    }
    Ok(channel(value))
}

/// Parses a `rgb()` alpha as a number or a percentage.
fn rgb_alpha(text: &str) -> Result<f64> {
    let text = text.trim();
    if let Some(percent) = text.strip_suffix('%') {
        let value: f64 = percent
            .trim()
            .parse()
            .with_context(|| format!("invalid rgb() alpha: {text}"))?;
        if !value.is_finite() {
            bail!("rgb() alpha is not finite");
        }
        return Ok((value / 100.0).clamp(0.0, 1.0));
    }
    let value: f64 = text
        .parse()
        .with_context(|| format!("invalid rgb() alpha: {text}"))?;
    if !value.is_finite() {
        bail!("rgb() alpha is not finite");
    }
    Ok(value.clamp(0.0, 1.0))
}

/// Rounds a channel from `0..=255` and clamps it.
fn channel(value: f64) -> u8 {
    let value = value.round().clamp(0.0, 255.0);
    // The clamp makes the cast exact for every input.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=255"
    )]
    let channel = value as u8;
    channel
}

/// Formats a color as a compact hex string, with alpha only when it matters.
fn format_color(red: u8, green: u8, blue: u8, alpha: f64) -> String {
    if alpha >= 0.999 {
        format!("#{red:02x}{green:02x}{blue:02x}")
    } else {
        format!(
            "#{red:02x}{green:02x}{blue:02x}{:02x}",
            channel(alpha * 255.0)
        )
    }
}

/// The basic CSS named colors, enough for an author mix.
fn named_color(name: &str) -> Option<Color> {
    let (red, green, blue) = match name {
        "black" => (0, 0, 0),
        "silver" => (192, 192, 192),
        "gray" | "grey" => (128, 128, 128),
        "white" => (255, 255, 255),
        "maroon" => (128, 0, 0),
        "red" => (255, 0, 0),
        "purple" => (128, 0, 128),
        "fuchsia" | "magenta" => (255, 0, 255),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "olive" => (128, 128, 0),
        "yellow" => (255, 255, 0),
        "navy" => (0, 0, 128),
        "blue" => (0, 0, 255),
        "teal" => (0, 128, 128),
        "aqua" | "cyan" => (0, 255, 255),
        "orange" => (255, 165, 0),
        "rebeccapurple" => (102, 51, 153),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "whitesmoke" => (245, 245, 245),
        _ => return None,
    };
    Some(Color {
        red,
        green,
        blue,
        alpha: 1.0,
    })
}

/// Builds the variable table that Ondine's aliases read.
fn theme_vars(theme: Theme) -> HashMap<String, String> {
    let background = hex(theme.background);
    let foreground = hex(theme.foreground);
    let muted = hex(theme.muted);
    let border = hex(theme.border);
    let accent = hex(theme.accent);
    let surface_alt = hex(mix(theme.foreground, 20.0, theme.background));

    let mut vars = HashMap::new();
    for (name, value) in [
        ("--ondine-bg", background.as_str()),
        ("--ondine-fg", foreground.as_str()),
        ("--ondine-muted", muted.as_str()),
        // Connector lines read as structure, not secondary text, so they
        // share the outline color instead of the muted text color.
        ("--ondine-line", border.as_str()),
        ("--ondine-surface", background.as_str()),
        ("--ondine-surface-alt", surface_alt.as_str()),
        ("--ondine-border", border.as_str()),
        ("--ondine-accent", accent.as_str()),
        ("--ondine-critical", accent.as_str()),
        ("--ondine-stroke", "2px"),
        ("--ondine-radius", "4"),
        ("--ondine-font", ROOT_FONT),
        ("--ondine-title-weight", "bold"),
        ("--ondine-heading-weight", "bold"),
        ("--ondine-bold-weight", "bold"),
    ] {
        vars.insert(name.to_owned(), value.to_owned());
    }

    // Series 1 carries the accent so the primary slice of a pie or the first
    // line of a chart stands out; the rest walk a monochrome ladder. Every
    // series must stay present: Ondine writes each fill as
    // `var(--ondine-series-N, color-mix(...))`, and a missing entry unfolds
    // the mix fallback into an expression this resolver rejects.
    vars.insert("--ondine-series-1".to_owned(), accent.clone());
    for (index, shade) in SERIES_SHADES.iter().enumerate() {
        let value = hex(mix(theme.foreground, f64::from(*shade), theme.background));
        vars.insert(format!("--ondine-series-{}", index + 2), value);
    }

    for (alias, target) in [
        ("--_bg", "--ondine-bg"),
        ("--_fg", "--ondine-fg"),
        ("--_m", "--ondine-muted"),
        ("--_l", "--ondine-line"),
        ("--_s", "--ondine-surface"),
        ("--_a", "--ondine-surface-alt"),
        ("--_b", "--ondine-border"),
        ("--_c", "--ondine-accent"),
        ("--_sw", "--ondine-stroke"),
        ("--_r", "--ondine-radius"),
        ("--_fm", "--ondine-font"),
        ("--_tw", "--ondine-title-weight"),
        ("--_hw", "--ondine-heading-weight"),
        ("--_w", "--ondine-bold-weight"),
    ] {
        if let Some(value) = vars.get(target).cloned() {
            vars.insert(alias.to_owned(), value);
        }
    }
    vars
}

/// Formats an sRGB triplet.
fn hex(color: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

/// Mixes `first` over `second` at `percent`, in gamma-encoded sRGB.
fn mix(first: [u8; 3], percent: f64, second: [u8; 3]) -> [u8; 3] {
    let weight = percent.clamp(0.0, 100.0) / 100.0;
    let blend = |first: u8, second: u8| -> u8 {
        channel(f64::from(first) * weight + f64::from(second) * (1.0 - weight))
    };
    [
        blend(first[0], second[0]),
        blend(first[1], second[1]),
        blend(first[2], second[2]),
    ]
}

/// Rejects elements and attributes that could reach the network or the host.
fn audit(svg: &str) -> Result<()> {
    let lower = svg.to_ascii_lowercase();
    for forbidden in [
        "<script",
        "<image",
        "<foreignobject",
        "<iframe",
        "<object",
        "<embed",
    ] {
        if lower.contains(forbidden) {
            bail!("diagram SVG contains a forbidden element: {forbidden}");
        }
    }

    for tag in start_tags(svg)? {
        for span in tag_attributes(tag) {
            let name = tag
                .get(span.name.clone())
                .unwrap_or("")
                .to_ascii_lowercase();
            let value = tag.get(span.value.clone()).unwrap_or("");

            if name == "href" || name == "xlink:href" {
                if !value.trim_start().starts_with('#') {
                    bail!("diagram SVG contains an external link");
                }
                continue;
            }
            if name != "style" && !DRAWING_ATTRIBUTES.contains(&name.as_str()) {
                continue;
            }
            if contains_ci(value, "var(") || contains_ci(value, "color-mix(") {
                bail!("diagram SVG keeps unresolved drawing CSS in {name}");
            }
            if contains_ci(value, "javascript:")
                || contains_ci(value, "expression(")
                || contains_ci(value, "@import")
            {
                bail!("diagram SVG keeps an unsafe declaration in {name}");
            }
            ensure_local_urls(value)?;
        }
    }
    Ok(())
}

/// Rejects a `url()` that points outside the document.
fn ensure_local_urls(value: &str) -> Result<()> {
    let mut at = 0;
    while let Some(found) = find_ci(value.get(at..).unwrap_or(""), "url(") {
        let start = at + found;
        let rest = value.get(start + 4..).unwrap_or("");
        if !rest.trim_start().starts_with('#') {
            bail!("diagram SVG contains a non-local url()");
        }
        at = start + 4;
    }
    Ok(())
}

/// Collects every start tag in the document.
fn start_tags(svg: &str) -> Result<Vec<&str>> {
    let mut tags = Vec::new();
    let mut at = 0;
    while at < svg.len() {
        let rest = svg.get(at..).unwrap_or("");
        if !rest.starts_with('<') {
            at = rest.find('<').map_or(svg.len(), |offset| at + offset);
            continue;
        }
        if rest.starts_with("<!--") {
            at = find_after(svg, at, "-->")?;
        } else if rest.starts_with("<![CDATA[") {
            at = find_after(svg, at, "]]>")?;
        } else if rest.starts_with("<?") {
            at = find_after(svg, at, "?>")?;
        } else if rest.starts_with("<!") || rest.starts_with("</") {
            at = find_after(svg, at, ">")?;
        } else {
            let end = tag_end(svg, at)?;
            tags.push(svg.get(at..end).unwrap_or(""));
            at = end;
        }
    }
    Ok(tags)
}

/// Splits `text` at a separator that is outside parentheses and quotes.
fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut quote = None;
    for (index, character) in text.char_indices() {
        match quote {
            Some(ending) => {
                if character == ending {
                    quote = None;
                }
            }
            None => match character {
                '\'' | '"' => quote = Some(character),
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                character if character == separator && depth == 0 => {
                    parts.push(text.get(start..index).unwrap_or(""));
                    start = index + character.len_utf8();
                }
                _ => {}
            },
        }
    }
    parts.push(text.get(start..).unwrap_or(""));
    parts
}

/// Finds `needle` outside parentheses and quotes.
fn find_top_level(text: &str, needle: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    for (index, character) in text.char_indices() {
        match quote {
            Some(ending) => {
                if character == ending {
                    quote = None;
                }
            }
            None => match character {
                '\'' | '"' => quote = Some(character),
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                character if character == needle && depth == 0 => return Some(index),
                _ => {}
            },
        }
    }
    None
}

/// Splits at the first comma outside parentheses and quotes.
fn split_first_comma(text: &str) -> (&str, Option<&str>) {
    match find_top_level(text, ',') {
        Some(index) => (
            text.get(..index).unwrap_or(""),
            Some(text.get(index + 1..).unwrap_or("")),
        ),
        None => (text, None),
    }
}

/// Finds the `)` that closes the `(` at `open`.
fn matching_paren(text: &str, open: usize) -> Result<usize> {
    let bytes = text.as_bytes();
    if bytes.get(open) != Some(&b'(') {
        bail!("expected '(' at byte {open}");
    }
    let mut depth = 0usize;
    let mut quote = None;
    let mut index = open;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(ending) => {
                if byte == ending {
                    quote = None;
                }
            }
            None => match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Ok(index);
                    }
                }
                _ => {}
            },
        }
        index += 1;
    }
    bail!("unterminated CSS function at byte {open}")
}

/// Finds `needle` in `haystack`, ignoring ASCII case.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&index| {
        haystack
            .get(index..index + needle.len())
            .is_some_and(|window| window.eq_ignore_ascii_case(needle))
    })
}

/// Finds the last occurrence of `needle`, ignoring ASCII case.
fn find_last_ci(haystack: &str, needle: &str) -> Option<usize> {
    let haystack = haystack.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    haystack.rfind(needle.as_str())
}

/// Whether `haystack` contains `needle`, ignoring ASCII case.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    find_ci(haystack, needle).is_some()
}

/// Escapes a replacement value for a double-quoted XML attribute.
fn push_escaped(out: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}' => {}
            _ => out.push(character),
        }
    }
}
