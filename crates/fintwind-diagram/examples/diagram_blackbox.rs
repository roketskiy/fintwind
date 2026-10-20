//! Black-box acceptance test for `src/md/diagram_compile.rs`.
//!
//! Run:
//!
//! ```text
//! cargo run --example diagram_blackbox -- C:/Users/Public/Temp/opencode/diagram-report.json
//! ```
//!
//! The example calls only the public `compile` API. It starts no process and
//! writes only the report. The report has no clock, path or machine data, so two
//! runs produce byte-identical files. Exit code 1 means a case failed.
//!
//! The module under test is included by path on purpose: this example must stay
//! independent of how the application registers it.

#[path = "../src/diagram_compile.rs"]
mod diagram_compile;

use std::fs;
use std::path::PathBuf;

use diagram_compile::{Diagram, Theme};
use regex::Regex;

const LIGHT: Theme = Theme {
    background: [255, 255, 255],
    foreground: [31, 35, 40],
    muted: [87, 96, 106],
    border: [208, 215, 222],
    accent: [9, 105, 218],
};

fn compile(source: &str, theme: Theme) -> anyhow::Result<Diagram> {
    diagram_compile::compile(source, theme, 1_735_689_600_000.0)
}

const DARK: Theme = Theme {
    background: [30, 30, 30],
    foreground: [212, 212, 212],
    muted: [154, 154, 154],
    border: [60, 60, 60],
    accent: [77, 170, 252],
};

/// The same drawing properties the module audits.
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

const FLOWCHART: &str = "\
flowchart LR
  A[Start] --> B{Check}
  B -->|yes| C[Done]
  B -->|no| A
";

const SEQUENCE: &str = "\
sequenceDiagram
  participant A as Alice
  participant B as Bob
  A->>B: Hello
  B-->>A: Hi
";

const CLASS: &str = "\
classDiagram
  class Animal {
    +String name
    +eat() void
  }
  Animal <|-- Dog
";

const STATE: &str = "\
stateDiagram-v2
  [*] --> Idle
  Idle --> Busy: start
  Busy --> Idle: stop
";

const ER: &str = "\
erDiagram
  CUSTOMER ||--o{ ORDER : places
  ORDER ||--|{ LINE_ITEM : contains
";

const GANTT: &str = "\
gantt
  title Fixed clock
  dateFormat YYYY-MM-DD
  section Work
  Task one :a1, 2026-01-01, 3d
  Task two :a2, 2026-01-05, 2d
";

const PIE: &str = "\
pie title Pets
  \"Dogs\" : 386
  \"Cats\" : 85
  \"Rats\" : 15
  \"Birds\" : 40
  \"Fish\" : 20
  \"Hamsters\" : 10
  \"Turtles\" : 8
  \"Rabbits\" : 6
";

const AUTHOR_COLORS: &str = "\
flowchart LR
  A[Red] --> B[Green]
  style A fill:#ff0000,stroke:#00ff00
";

const LABEL_LITERALS: &str = "\
flowchart LR
  A[\"var(--x)\"] --> B[\"color-mix(y)\"]
";

const IMAGE: &str = "\
flowchart LR
  A@{ img: \"https://example.com/x.png\", label: \"pic\" }
";

const LINK: &str = "\
flowchart LR
  A[Start] --> B[Done]
  click B \"https://example.com\"
";

const AUTHOR_CSS: &str = "\
flowchart LR
  A[Box] --> B[Other]
  style A fill:javascript:foo,stroke:#654321
";

#[derive(Default)]
struct Case {
    name: &'static str,
    problems: Vec<String>,
    notes: Vec<String>,
    width: f64,
    height: f64,
    svg_bytes: usize,
    svg_hash: String,
}

impl Case {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }

    fn check(&mut self, condition: bool, problem: impl Into<String>) {
        if !condition {
            self.problems.push(problem.into());
        }
    }

    fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }
}

fn main() {
    let Some(report_path) = std::env::args().nth(1) else {
        eprintln!("usage: diagram_blackbox <report.json>");
        std::process::exit(2);
    };
    let report_path = PathBuf::from(report_path);

    let mut cases = Vec::new();

    cases.push(success("flowchart", FLOWCHART, LIGHT, |case, svg| {
        case.check(svg.contains("<svg"), "SVG has no root element");
        check_portable_css(case, svg);
    }));

    cases.push(success("sequence", SEQUENCE, LIGHT, |case, svg| {
        case.check(svg.contains("Alice"), "sequence text is missing");
        check_portable_css(case, svg);
    }));

    cases.push(success("class", CLASS, LIGHT, |case, svg| {
        case.check(svg.contains("Animal"), "class text is missing");
        check_portable_css(case, svg);
    }));

    cases.push(success("state", STATE, LIGHT, |case, svg| {
        case.check(svg.contains("Idle"), "state text is missing");
        check_portable_css(case, svg);
    }));

    cases.push(success("er", ER, LIGHT, |case, svg| {
        case.check(svg.contains("CUSTOMER"), "ER text is missing");
        check_portable_css(case, svg);
    }));

    cases.push(success("gantt_fixed_clock", GANTT, LIGHT, |case, svg| {
        case.note("compile() uses a fixed now; source dates stay authoritative");
        case.check(svg.contains("Fixed clock"), "gantt title is missing");
        check_portable_css(case, svg);
    }));

    cases.push(success("complex_color_mix", PIE, LIGHT, |case, svg| {
        // Theme series colors are color-mix(fg N%, bg) results for N = 57, 86, 71.
        for expected in ["#7f8284", "#3e4246", "#606366"] {
            case.check(
                svg.contains(expected),
                format!("series color {expected} is missing"),
            );
        }
        check_portable_css(case, svg);
    }));

    let light = compile(FLOWCHART, LIGHT).map(|diagram| diagram.svg);
    cases.push(success("light_theme", FLOWCHART, LIGHT, |case, svg| {
        for expected in ["#ffffff", "#1f2328", "JetBrains Mono"] {
            case.check(
                svg.contains(expected),
                format!("light theme value {expected} is missing"),
            );
        }
        check_portable_css(case, svg);
    }));

    cases.push(success("dark_theme", FLOWCHART, DARK, |case, svg| {
        for expected in ["#1e1e1e", "#d4d4d4", "JetBrains Mono"] {
            case.check(
                svg.contains(expected),
                format!("dark theme value {expected} is missing"),
            );
        }
        if let Ok(light) = &light {
            case.check(
                light.as_str() != svg,
                "dark theme output equals light theme output",
            );
        }
        check_portable_css(case, svg);
    }));

    cases.push(success(
        "author_colors",
        AUTHOR_COLORS,
        LIGHT,
        |case, svg| {
            for expected in ["#ff0000", "#00ff00"] {
                case.check(
                    svg.contains(expected),
                    format!("author color {expected} was overwritten"),
                );
            }
            check_portable_css(case, svg);
        },
    ));

    cases.push(success(
        "label_literals",
        LABEL_LITERALS,
        LIGHT,
        |case, svg| {
            for literal in ["var(--x)", "color-mix(y)"] {
                case.check(
                    svg.contains(literal),
                    format!("label text {literal} was changed"),
                );
            }
            check_portable_css(case, svg);
        },
    ));

    cases.push(success("image_blocked", IMAGE, LIGHT, |case, svg| {
        check_absent(case, svg, &["<image", "example.com"]);
        check_portable_css(case, svg);
    }));

    cases.push(success("link_blocked", LINK, LIGHT, |case, svg| {
        check_absent(case, svg, &["href=", "<a ", "example.com"]);
        check_portable_css(case, svg);
    }));

    cases.push(success(
        "author_css_rejected",
        AUTHOR_CSS,
        LIGHT,
        |case, svg| {
            check_absent(case, svg, &["javascript:", "url("]);
            check_portable_css(case, svg);
        },
    ));

    cases.push(rejected("invalid_source", "not a diagram", "diagram type"));
    cases.push(rejected("oversized_source", &oversized(), "32768"));
    cases.push(rejected("edge_limit", &many_edges(), "512"));

    let mut failed = 0;
    let mut values = Vec::with_capacity(cases.len());
    for case in &cases {
        let passed = case.problems.is_empty();
        if !passed {
            failed += 1;
        }
        values.push(serde_json::json!({
            "name": case.name,
            "status": if passed { "pass" } else { "fail" },
            "problems": &case.problems,
            "notes": &case.notes,
            "width": case.width,
            "height": case.height,
            "svg_bytes": case.svg_bytes,
            "svg_fnv1a64": case.svg_hash,
        }));
    }

    let report = serde_json::json!({
        "version": 1,
        "tool": "diagram_blackbox",
        "summary": {
            "total": cases.len(),
            "passed": cases.len() - failed,
            "failed": failed,
        },
        "cases": values,
    });
    let text = serde_json::to_string_pretty(&report).expect("the report is serializable");
    if let Some(parent) = report_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).expect("create the report directory");
    }
    fs::write(&report_path, format!("{text}\n")).expect("write the report");

    println!(
        "diagram_blackbox: {} cases, {} failed; report {}",
        cases.len(),
        failed,
        report_path.display()
    );
    if failed > 0 {
        std::process::exit(1);
    }
}

/// Compiles, checks the contract, runs the case checks, and compiles twice.
fn success(
    name: &'static str,
    source: &str,
    theme: Theme,
    checks: impl FnOnce(&mut Case, &str),
) -> Case {
    let mut case = Case::new(name);
    match compile(source, theme) {
        Ok(diagram) => {
            fill(&mut case, &diagram);
            checks(&mut case, &diagram.svg);
            match compile(source, theme) {
                Ok(second) if second.svg == diagram.svg => {}
                Ok(_) => case
                    .problems
                    .push("a second compile produced a different SVG".to_owned()),
                Err(error) => case
                    .problems
                    .push(format!("a second compile failed: {error:#}")),
            }
        }
        Err(error) => case.problems.push(format!("compile failed: {error:#}")),
    }
    case
}

/// Expects a compile error whose message mentions `expected`.
fn rejected(name: &'static str, source: &str, expected: &str) -> Case {
    let mut case = Case::new(name);
    match compile(source, LIGHT) {
        Ok(diagram) => {
            fill(&mut case, &diagram);
            case.problems
                .push("compile succeeded, but an error was expected".to_owned());
        }
        Err(error) => {
            let message = format!("{error:#}");
            case.note(format!("rejected: {message}"));
            case.check(
                message.contains(expected),
                format!("error message does not mention {expected}"),
            );
        }
    }
    case
}

/// Records the size contract and a stable fingerprint.
fn fill(case: &mut Case, diagram: &Diagram) {
    case.width = diagram.width;
    case.height = diagram.height;
    case.svg_bytes = diagram.svg.len();
    case.svg_hash = fnv1a64(&diagram.svg);
    case.check(!diagram.svg.trim().is_empty(), "SVG is empty");
    case.check(
        diagram.width.is_finite()
            && diagram.height.is_finite()
            && diagram.width > 0.0
            && diagram.height > 0.0
            && diagram.width <= 100_000.0
            && diagram.height <= 100_000.0,
        format!("size {} x {} is not usable", diagram.width, diagram.height),
    );
    case.check(case.svg_bytes <= 1024 * 1024, "SVG is larger than 1 MiB");
}

/// Fails when a drawing attribute still holds CSS that usvg cannot read.
fn check_portable_css(case: &mut Case, svg: &str) {
    check_separators(case, svg);
    for (name, value) in attribute_values(svg) {
        if name != "style" && !DRAWING_ATTRIBUTES.contains(&name.as_str()) {
            continue;
        }
        let value = value.to_ascii_lowercase();
        if value.contains("var(") {
            case.problems.push(format!("attribute {name} keeps var()"));
        }
        if value.contains("color-mix(") {
            case.problems
                .push(format!("attribute {name} keeps color-mix()"));
        }
    }
}

/// Fails when a rewritten attribute lost the whitespace before its name.
fn check_separators(case: &mut Case, svg: &str) {
    for piece in svg.split('<').skip(1) {
        if piece.starts_with('/') || piece.starts_with('!') || piece.starts_with('?') {
            continue;
        }
        let Some(tag) = piece.split('>').next() else {
            continue;
        };
        let bytes = tag.as_bytes();
        let mut quoted = None;
        for index in 0..bytes.len().saturating_sub(1) {
            match quoted {
                Some(quote) if bytes[index] == quote => {
                    quoted = None;
                    let next = bytes[index + 1];
                    if next.is_ascii_alphabetic() || matches!(next, b'_' | b':') {
                        case.problems
                            .push("attributes are not separated by whitespace".to_owned());
                        return;
                    }
                }
                None if matches!(bytes[index], b'"' | b'\'') => quoted = Some(bytes[index]),
                _ => {}
            }
        }
    }
}

/// Fails when any needle appears, ignoring case.
fn check_absent(case: &mut Case, svg: &str, needles: &[&str]) {
    let lower = svg.to_ascii_lowercase();
    for needle in needles {
        case.check(
            !lower.contains(&needle.to_ascii_lowercase()),
            format!("SVG contains {needle}"),
        );
    }
}

/// Reads name/value pairs from start tags. Text nodes are not read.
fn attribute_values(svg: &str) -> Vec<(String, String)> {
    let attribute = Regex::new(r#"([A-Za-z_:][-A-Za-z0-9_:.]*)\s*=\s*"([^"]*)""#)
        .expect("the attribute pattern is valid");
    let mut values = Vec::new();
    for piece in svg.split('<').skip(1) {
        if piece.starts_with('/') || piece.starts_with('!') || piece.starts_with('?') {
            continue;
        }
        let Some(tag) = piece.split('>').next() else {
            continue;
        };
        for capture in attribute.captures_iter(tag) {
            let name = capture
                .get(1)
                .map(|value| value.as_str().to_ascii_lowercase())
                .unwrap_or_default();
            let value = capture
                .get(2)
                .map(|value| value.as_str().to_owned())
                .unwrap_or_default();
            values.push((name, value));
        }
    }
    values
}

/// A stable 64-bit FNV-1a fingerprint. No hash implementation is shared with the module.
fn fnv1a64(text: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn oversized() -> String {
    let mut source = String::from("flowchart LR\n");
    source.push_str(&"A --> B\n".repeat(5_000));
    source
}

fn many_edges() -> String {
    let mut source = String::from("flowchart LR\n");
    for index in 0..600 {
        source.push_str(&format!("N{index} --> N{}\n", index + 1));
    }
    source
}
