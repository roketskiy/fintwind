//! Paint-only privacy markings. The OpenCode plugin still owns outbound replacement.

use std::{collections::HashMap, ops::Range, sync::LazyLock};

use aho_corasick::{AhoCorasick, MatchKind};
use fancy_regex::Regex;
use gpui::{Hsla, TextRun, UnderlineStyle, px};

use crate::md::parser::{self, Block, InlineRun};

struct Rules {
    tokens: Regex,
    provider_tokens: Regex,
    webhook: Regex,
    generic_key: Regex,
    generic_key_excluded_name: Regex,
    generic_key_plain_value: Regex,
    email: Regex,
    key_marker: Regex,
    key_body: Regex,
    field: Regex,
    field_value: Regex,
    password_name: Regex,
    uri_credential: Regex,
    auth_token: Regex,
    weak_name: Regex,
    weak_value: Regex,
    placeholder: Regex,
    nearby_name: Regex,
    non_secret_value: Regex,
}

// JavaScript's whitespace includes BOM, but excludes U+0085. Its digits and
// word boundaries are ASCII in these expressions.
const JS_WHITESPACE: &str = "\t\n\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

static RULES: LazyLock<Rules> = LazyLock::new(|| {
    let rules: HashMap<String, String> =
        serde_json::from_str(include_str!("../../../resources/prompt-privacy-rules.json"))
            .expect("valid built-in prompt privacy rules");
    let compile = |key: &str, flags: &str| {
        let source = rules[key]
            .replace(r"\d", "[0-9]")
            // Word boundaries here precede the literal `authorization`.
            .replace(r"\b", r"(?<!(?-i:[A-Za-z0-9_]))")
            .replace(r"\s", JS_WHITESPACE);
        Regex::new(&format!("{flags}{source}")).unwrap_or_else(|error| panic!("{key}: {error}"))
    };
    Rules {
        tokens: compile("tokens", ""),
        provider_tokens: compile("providerTokens", ""),
        webhook: compile("webhook", ""),
        generic_key: compile("genericKey", "(?i)"),
        generic_key_excluded_name: compile("genericKeyExcludedName", "(?i)"),
        generic_key_plain_value: compile("genericKeyPlainValue", ""),
        email: compile("email", ""),
        key_marker: compile("keyMarker", ""),
        key_body: compile("keyBody", ""),
        field: compile("field", "(?i)"),
        field_value: compile("fieldValue", ""),
        password_name: compile("passwordName", "(?i)"),
        uri_credential: compile("uriCredential", "(?i)"),
        auth_token: compile("authToken", "(?i)"),
        weak_name: compile("weakName", "(?i)"),
        weak_value: compile("weakValue", ""),
        placeholder: compile("placeholder", "(?i)"),
        nearby_name: compile("nearbyName", "(?i)"),
        non_secret_value: compile("nonSecretValue", ""),
    }
});

fn is_field_value(value: &str, name: &str, rules: &Rules) -> bool {
    let value =
        value.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
    let password = rules.password_name.is_match(name).unwrap_or(false);
    value.encode_utf16().take(8).count() >= if password { 1 } else { 8 }
        && !rules.placeholder.is_match(value).unwrap_or(false)
        && (!rules.weak_name.is_match(name).unwrap_or(false)
            || rules.weak_value.is_match(value).unwrap_or(false))
}

fn key_entropy(value: &str) -> f64 {
    let mut counts = [0u16; 256];
    for byte in value.bytes() {
        counts[usize::from(byte)] += 1;
    }
    counts
        .into_iter()
        .filter(|&count| count > 0)
        .map(|count| {
            let p = f64::from(count) / value.len() as f64;
            -p * p.log2()
        })
        .sum()
}

struct Hit<'a> {
    range: Range<usize>,
    original: Option<String>,
    name: Option<&'a str>,
    strength: u8,
    expandable: bool,
}

// A value cut by a pattern's length limit is extended to the whole token, so
// the marking never leaves the tail of a secret unmarked. A single trailing
// padding equals sign belongs to a long value too.
fn expand<'a>(text: &str, mut hit: Hit<'a>) -> Hit<'a> {
    if !hit.expandable || hit.original.is_some() {
        return hit;
    }
    let bytes = text.as_bytes();
    let value_byte =
        |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'~' | b'-');
    let mut start = hit.range.start;
    let mut end = hit.range.end;
    while start > 0 && value_byte(bytes[start - 1]) {
        start -= 1;
    }
    while end < bytes.len() && value_byte(bytes[end]) {
        end += 1;
    }
    if text[start..end].encode_utf16().count() >= 20
        && bytes.get(end) == Some(&b'=')
        && bytes.get(end + 1) != Some(&b'=')
    {
        end += 1;
    }
    hit.range = start..end;
    hit
}

// A JWT-shaped candidate must still parse as a JWT header. The other formats
// are fixed by their prefix and length, so only this one needs decoding.
fn structured(spelling: &str) -> bool {
    if !spelling.starts_with("eyJ") {
        return true;
    }
    let parts: Vec<&str> = spelling.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    use base64::Engine as _;
    let Ok(header) = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[0].trim_end_matches('='))
    else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&header) else {
        return false;
    };
    value
        .as_object()
        .is_some_and(|object| object.get("alg").is_some_and(|alg| alg.is_string()))
}

fn plain_word(spelling: &str) -> bool {
    !spelling.is_empty()
        && spelling.encode_utf16().count() < 24
        && spelling.bytes().all(|b| b.is_ascii_lowercase())
}

fn value_shaped(spelling: &str) -> bool {
    if spelling.encode_utf16().count() < 12 {
        return false;
    }
    let classes: usize = [
        spelling.bytes().any(|b| b.is_ascii_uppercase()),
        spelling.bytes().any(|b| b.is_ascii_lowercase()),
        spelling.bytes().any(|b| b.is_ascii_digit()),
        spelling.bytes().any(|b| !b.is_ascii_alphanumeric()),
    ]
    .into_iter()
    .map(usize::from)
    .sum();
    classes >= 2 && key_entropy(spelling) >= 3.0
}

// Score a candidate from the value alone and from the words just before it,
// mirroring accepted() in the OpenCode detector. Strong formats skip
// scoring; identifiers and timestamps are never secrets unless a password
// field names them.
fn accepted(text: &str, hit: &Hit<'_>, r: &Rules) -> bool {
    let spelling = &text[hit.range.clone()];
    if !structured(spelling) {
        return false;
    }
    let password = hit
        .name
        .is_some_and(|name| r.password_name.is_match(name).unwrap_or(false));
    if !password
        && (r.non_secret_value.is_match(spelling).unwrap_or(false) || plain_word(spelling))
    {
        return false;
    }
    if hit.strength >= 3 {
        return true;
    }
    let mut score = hit.strength;
    if hit
        .name
        .is_some_and(|name| r.nearby_name.is_match(name).unwrap_or(false))
    {
        score += 1;
    }
    if value_shaped(spelling) {
        score += 1;
    }
    let line_start = text[..hit.range.start].rfind('\n').map_or(0, |i| i + 1);
    let prefix = &text[line_start..hit.range.start];
    let mut units = 0;
    let mut context_start = prefix.len();
    for (index, ch) in prefix.char_indices().rev() {
        let width = ch.len_utf16();
        if units + width > 48 {
            break;
        }
        units += width;
        context_start = index;
    }
    if r.nearby_name.is_match(&prefix[context_start..]).unwrap_or(false) {
        score += 2;
    }
    score >= 3
}

// Keep the context checks in step with discover() in the OpenCode plugin.
fn discover(text: &str) -> impl Iterator<Item = (Range<usize>, Option<String>)> {
    let r = &*RULES;
    let mut hits = Vec::new();
    hits.extend(r.tokens.find_iter(text).flatten().map(|m| Hit {
        range: m.range(),
        original: None,
        name: None,
        strength: 3,
        expandable: true,
    }));
    hits.extend(r.provider_tokens.find_iter(text).flatten().map(|m| Hit {
        range: m.range(),
        original: None,
        name: None,
        strength: 3,
        expandable: true,
    }));
    hits.extend(r.webhook.captures_iter(text).flatten().map(|m| Hit {
        range: m.get(1).unwrap().range(),
        original: None,
        name: None,
        strength: 3,
        expandable: false,
    }));
    for m in r.generic_key.captures_iter(text).flatten() {
        let value = m.get(2).unwrap();
        if !r.generic_key_excluded_name.is_match(&m[1]).unwrap_or(false)
            && !r.placeholder.is_match(value.as_str()).unwrap_or(false)
            && !r
                .generic_key_plain_value
                .is_match(value.as_str())
                .unwrap_or(false)
            && key_entropy(value.as_str()) >= 3.1
        {
            hits.push(Hit {
                range: value.range(),
                original: None,
                name: Some(m.get(1).unwrap().as_str()),
                strength: 1,
                expandable: true,
            });
        }
    }
    for m in r.auth_token.captures_iter(text).flatten() {
        let token = m.get(1).or_else(|| m.get(2)).unwrap();
        if is_field_value(token.as_str(), "", r) {
            hits.push(Hit {
                range: token.range(),
                original: None,
                name: None,
                strength: 2,
                expandable: true,
            });
        }
    }
    if text.contains('@') {
        let mut credentials = Vec::new();
        for m in r.uri_credential.captures_iter(text).flatten() {
            let pair = m.get(1).unwrap().range();
            if is_field_value(&m[2], "password", r) {
                hits.push(Hit {
                    range: pair.clone(),
                    original: None,
                    name: Some("password"),
                    strength: 3,
                    expandable: true,
                });
            }
            credentials.push(pair);
        }
        let mut credentials = credentials.into_iter().peekable();
        for email in r.email.find_iter(text).flatten() {
            while credentials
                .peek()
                .is_some_and(|pair| pair.end <= email.start())
            {
                credentials.next();
            }
            if credentials
                .peek()
                .is_none_or(|pair| pair.start > email.start())
            {
                hits.push(Hit {
                    range: email.range(),
                    original: None,
                    name: None,
                    strength: 3,
                    expandable: false,
                });
            }
        }
    }
    if text.contains("PRIVATE KEY") {
        let mut open = None;
        for m in r.key_marker.captures_iter(text).flatten() {
            let marker = m.get(0).unwrap();
            if &m[1] == "BEGIN" {
                open = Some((marker.range(), m.get(2).unwrap().as_str()));
            } else if let Some((start, kind)) = open.take() {
                let body = &text[start.end..marker.start()];
                if kind == &m[2]
                    && body.encode_utf16().count() >= 32
                    && (kind == "PGP PRIVATE KEY BLOCK"
                        || r.key_body.is_match(body).unwrap_or(false))
                {
                    hits.push(Hit {
                        range: start.start..marker.end(),
                        original: None,
                        name: None,
                        strength: 3,
                        expandable: false,
                    });
                }
            }
        }
    }
    for m in r.field.captures_iter(text).flatten() {
        let field = m.get(0).unwrap();
        let offset = field.end();
        let Ok(Some(value)) = r.field_value.captures(&text[offset..]) else {
            continue;
        };
        let spelling = (1..=4).find_map(|index| value.get(index)).unwrap();
        let name = m
            .name("name")
            .or_else(|| m.name("flag"))
            .map_or("", |m| m.as_str());
        let original = if !name.is_empty()
            && value.get(1).is_some_and(|m| m.as_str().contains('\\'))
            && field
                .start()
                .checked_sub(1)
                .is_some_and(|i| text.as_bytes()[i] == b'"')
            && field.as_str()[name.len()..].trim() == "\":"
        {
            serde_json::from_str::<String>(&value[0]).ok()
        } else {
            None
        };
        if is_field_value(original.as_deref().unwrap_or(spelling.as_str()), name, r) {
            hits.push(Hit {
                range: offset + spelling.start()..offset + spelling.end(),
                original,
                name: if name.is_empty() { None } else { Some(name) },
                strength: 3,
                expandable: true,
            });
        }
    }
    let mut out = Vec::new();
    for hit in hits {
        let hit = expand(text, hit);
        if accepted(text, &hit, r) {
            out.push((hit.range, hit.original));
        }
    }
    out.into_iter()
}

pub fn ranges(text: &str) -> Vec<Range<usize>> {
    merge(discover(text).map(|(range, _)| range).collect())
}

fn merge(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_unstable_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut().filter(|last| last.end >= range.start) {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

#[derive(Default)]
pub struct History(HashMap<String, Vec<Range<usize>>>);

impl History {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn scan(source: &str) -> Self {
        // Find values in the source, not in isolated display runs: formatting
        // can split a field from its value or hide its quote delimiters.
        let hits: Vec<_> = discover(source).collect();
        if hits.is_empty() {
            return Self::default();
        }
        let source_ranges = merge(hits.iter().map(|(range, _)| range.clone()).collect());
        let mut patterns = Vec::new();
        for (range, original) in hits {
            let spelling = &source[range];
            // Multiline keys can straddle Markdown paragraph boundaries.
            if spelling.contains('\n') {
                patterns.extend(
                    spelling
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(str::to_owned),
                );
            }
            patterns.push(spelling.to_owned());
            patterns.extend(original.filter(|value| !value.is_empty()));
            // Markdown can remove delimiters, decode entities and unescape
            // punctuation. Include the visible spelling when it differs.
            if spelling.contains(['*', '_', '~', '`', '\\', '&']) {
                for top in parser::parse(spelling).blocks {
                    visit_text(top.block, &mut |text| {
                        if !text.is_empty() && text != spelling {
                            patterns.push(text);
                        }
                    });
                }
            }
        }
        patterns.sort_unstable();
        patterns.dedup();
        let matcher = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(patterns)
            .expect("valid literal privacy values");
        let mut map = HashMap::new();
        let mut mark = |text: String| {
            let mut contextual = None;
            let ranges: Vec<_> = matcher
                .find_iter(&text)
                .filter(|m| {
                    // A short password needs a field in the displayed text,
                    // too; do not underline every matching letter in prose.
                    if text[m.range()].encode_utf16().take(8).count() == 8 {
                        return true;
                    }
                    let contextual = contextual.get_or_insert_with(|| ranges(&text));
                    let index = contextual.partition_point(|range| range.end <= m.start());
                    contextual
                        .get(index)
                        .is_some_and(|range| range.start < m.end())
                })
                .map(|m| m.range())
                .collect();
            if !ranges.is_empty() {
                map.insert(text, ranges);
            }
        };
        for top in parser::parse(source).blocks {
            visit_text(top.block, &mut mark);
        }
        map.insert(source.to_owned(), source_ranges);
        Self(map)
    }

    pub fn ranges(&self, text: &str) -> &[Range<usize>] {
        self.0.get(text).map_or(&[], Vec::as_slice)
    }
}

fn visit_text(block: Block, visit: &mut impl FnMut(String)) {
    let flatten = |runs: Vec<InlineRun>| runs.into_iter().map(|run| run.text).collect();
    match block {
        Block::Paragraph { runs } | Block::Heading { runs, .. } => visit(flatten(runs)),
        Block::CodeBlock { code, .. } | Block::DisplayMath { latex: code } => visit(code),
        Block::BlockQuote { children } => {
            for child in children {
                visit_text(child, visit);
            }
        }
        Block::List { items, .. } => {
            for child in items.into_iter().flat_map(|item| item.blocks) {
                visit_text(child, visit);
            }
        }
        Block::Table { header, rows, .. } => {
            for cell in header.into_iter().chain(rows.into_iter().flatten()) {
                visit(flatten(cell));
            }
        }
        Block::Image { .. } | Block::Rule => {}
    }
}

pub fn style_runs(runs: Vec<TextRun>, ranges: &[Range<usize>], color: Hsla) -> Vec<TextRun> {
    if ranges.is_empty() {
        return runs;
    }
    let mut out = Vec::with_capacity(runs.len());
    let (mut offset, mut index) = (0, 0);
    for run in runs {
        let end = offset + run.len;
        while offset < end {
            while ranges.get(index).is_some_and(|range| range.end <= offset) {
                index += 1;
            }
            let mut styled = run.clone();
            let next = match ranges.get(index) {
                Some(range) if range.start <= offset => {
                    styled.background_color.get_or_insert(color.opacity(0.16));
                    styled.underline.get_or_insert(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.0),
                        wavy: false,
                    });
                    end.min(range.end)
                }
                Some(range) => end.min(range.start),
                None => end,
            };
            styled.len = next - offset;
            out.push(styled);
            offset = next;
        }
    }
    out
}
