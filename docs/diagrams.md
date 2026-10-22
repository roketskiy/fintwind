# Mermaid diagrams

Fintwind draws `mermaid` code fences as native SVG. The compiler is
[Ondine](https://github.com/wevm/ondine) (crate `ondine`, package `compiler`,
MIT), pinned by revision in `crates/fintwind-diagram/Cargo.toml`. Ondine is
pure Rust: no DOM, no JavaScript, no network. This module turns its themed SVG
into a document that the native SVG renderer (resvg/usvg) can draw without CSS
custom properties.

Code:

- `crates/fintwind-diagram/src/diagram_compile.rs` — limits, compile call,
  theme materialization, output audit. The compiler lives in its own crate so
  two consumers call one identical `compile`:
  - the app renderer (`src/md/diagram.rs`), which rasterizes the SVG; and
  - the daemon's mermaid check endpoint (`crates/fintwind-core/src/mermaid_check.rs`),
    which validates sources for the model, so a check can never pass on a
    source the renderer would reject.
  Public API:
  - `struct Theme { background, foreground, muted, border, accent: [u8; 3] }`
    (`Clone, Copy, Debug, Eq, Hash, PartialEq`)
  - `struct Diagram { svg: String, width: f64, height: f64 }`
  - `fn compile(source: &str, theme: Theme, now_ms: f64) -> anyhow::Result<Diagram>`
- `crates/fintwind-diagram/examples/diagram_blackbox.rs` — the black-box
  acceptance test. It prints a deterministic JSON report to the path given on
  the command line.
- `crates/fintwind-core/examples/mermaid_check_e2e.rs` — the black-box
  acceptance for the check endpoint: real `serve`, real handshake routing,
  token refusals, valid and invalid sources, size limits, several checks on
  one connection.

Ondine emits `var(--ondine-*)`, private aliases (`--_fg`, `--_sw`, …) and
`color-mix(in srgb, …)`. usvg supports none of them. The module resolves them
before the SVG leaves the compiler.

## Failure list (written before the implementation)

Each entry is a way the feature can fail, the countermeasure, and the test that
must show the countermeasure works. The list is the design, not a record of
past bugs.

| # | Failure | Countermeasure | Acceptance case |
|---|---|---|---|
| F1 | Source is huge and stalls the UI | Reject more than 32 KiB before parsing; pass `limit: 32768` too | `oversized_source` |
| F2 | Source is not a diagram / unsupported type | Return Ondine's typed error as `anyhow::Error`; render nothing | `invalid_source` |
| F3 | Syntax or semantic error | Same path, keep line/column in the message | `invalid_source` |
| F4 | Output is huge | Ondine `outputLimit: 1 MiB`; check the post-processed SVG too | every successful case |
| F5 | Edge explosion (`A & B --> C & D`) | `maxEdges: 512` | `edge_limit` |
| F6 | Non-finite, zero, negative or absurd width/height | Reject unless both are finite, `> 0` and `<= 100_000` | every successful case |
| F7 | Gantt changes on theme/DPI switches or uses an arbitrary historical date | Capture the current UTC day once per source; acceptance uses an explicit fixed date | `gantt_fixed_clock` |
| F8 | `var(--ondine-bg)` is ignored by usvg, so fills disappear | Replace every drawing `var()` with a concrete value from the theme | all cases |
| F9 | Nested `color-mix()`, or `var()` inside it, is ignored | Resolve vars first, then fold the innermost `color-mix()` repeatedly; fail on an unsupported mix form | `complex_color_mix`, `pie_series` |
| F10 | A blind string replace rewrites label text (`var(--x)` is legal label text) | Scan tags; rewrite only attribute values. Text nodes, `<title>` and `<desc>` are copied byte for byte | `label_literals` |
| F11 | Theme overwrites author colors | Only values that contain `var(` or `color-mix(` are rewritten; explicit author colors pass through | `author_colors` |
| F12 | Author CSS injects `url()`, `javascript:` or `expression()` | Ondine drops those declarations; the output audit rejects external `url()` and non-fragment links | `author_css_rejected` |
| F13 | An author embeds an external image or link | `images: false`, `links: false`; audit rejects `<image`, `<script`, `<foreignObject` and any non-`#` href | `image_blocked`, `link_blocked` |
| F14 | Dark theme is unreadable or identical to light | All five theme fields drive their aliases; dark output must differ and contain the dark hex colors | `light_theme`, `dark_theme` |
| F15 | Proportional font breaks Ondine's monospace measurements | Root family is `JetBrains Mono, monospace` | `light_theme`, `dark_theme` |
| F16 | Private `--_*` declarations leak into the SVG | Materialize, then remove custom-property declarations from `style` | every successful case |
| F17 | Invalid XML from unescaped replacement values | Rebuild rewritten attributes with `&`, `<`, `>`, `"` escaped; keep all other attributes byte for byte | all cases, second compile |
| F18 | Malformed SVG makes the scanner panic | Every slice is checked; unterminated tags and parens return `Err`; no `unwrap` on input-derived data | review |
| F19 | Acceptance output depends on hash order or the wall clock | Theme lookup is a map, but output order follows the input document; acceptance passes an explicit fixed `now` | every successful case recompiles; byte-identical reports |
| F20 | Several diagrams on one page share SVG ids | Only Ondine's `title`/`desc` ids exist; they are scoped to one SVG document. Host wraps each diagram separately | out of scope here |
| F21 | Source can request a local file or a network fetch | Ondine's URL sanitizer; `images`/`links` off; no fonts or images are embedded or fetched | `image_blocked`, `link_blocked` |
| F22 | Label text is treated as markup | Ondine escapes `&`, `<`, `>` in text; the audit only inspects tags, never text | `label_literals` |
| F23 | A series color goes missing from the theme table | Ondine writes every series fill as `var(--ondine-series-N, color-mix(...))`; a missing entry unfolds the mix fallback into a form the resolver rejects. Series 1..=8 must all be present | `complex_color_mix` |
| F24 | East Asian labels fall back to a thin display face and blur when a wide diagram is drawn narrower | Prefer curated screen UI faces per script in the SVG font resolver, keeping the base weight and style | `cjk` compiles and draws; the picked face is verified out of band |

## Decisions

### Limits

- Source: 32 KiB (UTF-8 bytes).
- Output: 1 MiB.
- Flowchart edges: 512 (Ondine's `maxEdges` option; other diagram types still
  have the source, output and raster budgets, not a universal edge counter).
- Width and height: finite, `> 0`, `<= 100_000`.
- `now`: the current UTC day, captured once when a diagram source is prepared.
  Theme and DPI changes reuse that date. The black-box compiler example calls
  `compile` with 2025-01-01 only to make its report reproducible; production
  never uses that arbitrary historical date as today.

### Theme mapping

`Theme` has five colors. Ondine's aliases map as follows:

| Variable | Value |
|---|---|
| `--ondine-bg`, `--ondine-surface` | `background` |
| `--ondine-fg` | `foreground` |
| `--ondine-muted` | `muted` |
| `--ondine-line` | `border` (lines read as structure, not secondary text) |
| `--ondine-border` | `border` |
| `--ondine-accent`, `--ondine-critical` | `accent` |
| `--ondine-surface-alt` | `color-mix(fg 20%, bg)` |
| `--ondine-series-1` | `accent` (the primary slice or line stands out) |
| `--ondine-series-2..8` | `fg` at 57/86/50/79/64/93/71 % over `bg` |
| `--ondine-stroke` | `2px` |
| `--ondine-radius` | `4` |
| `--ondine-font` | `JetBrains Mono, monospace` |
| title/heading/bold weight | `bold` |

An unknown `var()` uses its fallback. An unknown `var()` without a fallback
becomes the theme foreground: the output must not keep an unresolved value, and
failing the whole diagram for an author typo is worse than a visible color.

### East Asian text fallback

Ondine measures every label with monospace metrics (0.6 em per Latin glyph,
1.2 em per East Asian glyph), so the root family must stay monospace and the
SVG cannot simply ask for a proportional CJK face. A glyph that the root family
lacks is resolved by usvg through the font database in registration order, which
on a typical Windows install lands on the thin display face DengXian; its
strokes blur when a wide diagram is drawn narrower than its natural width.

The native SVG renderer therefore resolves East Asian characters itself before
the default search: it prefers a screen-oriented UI face for the character's
script (Microsoft YaHei UI for Han and CJK punctuation, Yu Gothic UI for kana,
Malgun Gothic for Hangul, with PingFang, Noto Sans CJK and WenQuanYi shapes of
the same names for macOS and Linux), preferring the base face's weight and
style so bold labels stay bold. Latin text, symbols and emoji keep the document
family and the existing emoji preference. This lives in `vendor/gpui-pre` next
to the emoji fallback and applies to every SVG the app draws.

### Attribution and escaping

The theme resolver and the SVG audit work on a small XML tokenizer. A token is
either text, a comment, or a tag. Only start-tag attributes named `style` or a
drawing/text property (`fill`, `stroke`, `color`, `stop-color`, `flood-color`,
`stroke-width`, `font-family`, `font-weight`, `font-size`, `rx`, `ry`,
`marker-start|mid|end`) are candidates. Text between tags is never read for
`var(` or `color-mix(`, so a label such as `var(--_fg)` stays literal.

`style` declarations are split at top-level `;` and `:`. Local custom
properties (`--name: value`) are resolved and then removed; other declarations
keep their order. A value that still contains `var(` or `color-mix(` after
resolution fails the compile.

`color-mix(in srgb, A p%, B)` is folded in gamma-encoded sRGB, which is what
CSS specifies. One missing percentage is `100 - p`; both missing is 50/50;
percentages that do not sum to 100 are normalized. Hex, `rgb()`, `rgba()`,
`transparent` and the basic CSS named colors are accepted. Any other mix form
(for example `in oklab`) is an error.

## Mermaid check

The daemon exposes `ws://127.0.0.1:<port>/v1/mermaid-check`, a restricted
endpoint for the private OpenCode plugin only:

- `crates/fintwind-protocol/src/mermaid.rs` owns the versioned wire: a
  `Hello{version, token}` handshake, `Check{requestId, source}` calls and one
  bounded `Result{ok, error, width, height}` per call.
- `crates/fintwind-core/src/mermaid_check.rs` installs the endpoint (a
  per-daemon random token, never the main daemon token), answers the checks
  with the shared compiler, and hands the capability to private OpenCode
  processes through `FINTWIND_MERMAID_CHECK_ADDRESS` / `FINTWIND_MERMAID_CHECK_TOKEN`.
- `resources/opencode-context-plugin.ts` registers the `fintwind_mermaid_check`
  tool when the capability exists, keeps it out of every session whose
  creation metadata has no Mermaid prompt version, and returns the compiler's
  line-and-column error to the model verbatim. The permission default is
  `allow`: the check is read-only compilation.
- Prompts `v2` and `v3` tell the model to validate each diagram it
  writes and fix the reported errors. A failure the fence shows the same
  message above the source, so a broken diagram is never silent.

## Updating Ondine

Upstream is young and moves fast. The mechanical half is scripted; the
judgment is not:

```sh
bun scripts/update-ondine.ts
cargo update -p compiler
cargo run --locked --offline --package fintwind-diagram --example diagram_blackbox -- C:/Users/Public/Temp/opencode/diagram-report.json
```

Run the blackbox twice and compare the reports byte for byte. A changed
fingerprint means the rendered output changed: review the diff on purpose,
update the theme mapping notes above when it is wanted, then render a
flowchart, a sequence, a pie and a Gantt in the app before committing.

## Acceptance

Build and run (the parent build owns `cargo`):

```sh
cargo run --locked --offline --package fintwind-diagram --example diagram_blackbox -- C:/Users/Public/Temp/opencode/diagram-report.json
cargo run --locked --offline --package fintwind-core --example mermaid_check_e2e -- C:/Users/Public/Temp/opencode/mermaid-check-report.json
```

The report is reproducible: it contains no clock, path, duration or machine
data. Run it twice and compare the files byte for byte. Exit code is non-zero
if a case fails.

Cases:

| Case | Expectation |
|---|---|
| `flowchart`, `sequence`, `class`, `state`, `er` | compile succeeds, non-empty SVG, sane size |
| `gantt_fixed_clock` | compile succeeds twice with identical output; source dates stay authoritative |
| `complex_color_mix` | pie series colors resolve; no `color-mix(` in drawing attributes |
| `light_theme`, `dark_theme` | different SVG; theme hex colors present; dark background present; `JetBrains Mono` in the root style |
| `author_colors` | `#ff0000` / `#00ff00` survive |
| `label_literals` | label text `var(--x)` and `color-mix(y)` survives exactly |
| `image_blocked` | no `<image`, no `example.com` |
| `link_blocked` | no `<a`, no `href=`, no `example.com` |
| `author_css_rejected` | no `url(`, no `javascript:` |
| `invalid_source` | `Err` |
| `oversized_source` | `Err` before parsing |
| `edge_limit` | `Err` past 512 edges |

Every successful case also asserts, in the example:

- the same source compiles to a byte-identical SVG the second time;
- a separate scan of start-tag attributes finds no `var(` or `color-mix(`;
- width and height are finite, positive and at most 100 000;
- the SVG is at most 1 MiB.

The report stores the size, byte count and an FNV-1a fingerprint for each case,
so a run is repeatable and a diff is visible.

Not covered here, owned by the parent:

- Bundled font registration and the East Asian fallback preference in the
  native renderer are shared with GPUI; this module only fixes the root family.
- Turning the SVG into a texture, caching, invalidation and markdown layout.
- Visual comparison against Mermaid for all 33 diagram types. This module
  accepts every type Ondine supports; the acceptance set samples one
  representative per family.
