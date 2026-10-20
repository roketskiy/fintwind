# Native diagram acceptance

Failure cases to cover before implementation:

- SVG CSS variables or color-mix expressions silently lose fills and strokes.
- Bundled fonts are registered with GPUI text shaping but missing from SVG text.
- Theme switches reuse stale colors; monitor changes reuse undersampled images.
- Invalid or oversized source repeatedly compiles every frame.
- Pending compilation, failure, and explicit source mode lose the original source.
- Long reasoning splits one diagram into independent, invalid fragments.
- Diagram completion does not update visible list geometry.
- Embedded image references cause file or network access during SVG decoding.
- Compilation or font enumeration blocks the UI thread.
- Large SVG dimensions allocate an unbounded bitmap.
- Old work replaces the image for newer source or theme.

The module-level black-box examples exercise actual Ondine compilation and GPUI
SVG decoding without opening windows, starting OpenCode, or touching user data.
They do not prove desktop layout, keyboard behavior, or streaming scroll anchors.
Those remain native application acceptance items; no visual test is run by default.

Run from the repository root with a bounded command timeout:

```text
cargo run --locked --example diagram_blackbox -- target/diagram-acceptance/compiler.json
cargo run --locked --example diagram_raster_blackbox -- target/diagram-acceptance/raster
```

The second command writes SVG samples and a JSON report for actual native raster
decoding in both themes and at two display densities. It also exercises the
asset-based loading of the four bundled JetBrains Mono faces. It opens no UI.

The pinned GPUI list implementation measures visible rows every frame
(`vendor/gpui-pre/gpui-pre-0.3.8/src/elements/list.rs`, `layout_items`, lines
1073-1078). The image cache therefore notifies only requesting views; it does not
invalidate every historical row when a diagram finishes. Off-screen rows update
when they next enter the viewport.

## Current verification

- `cargo check --locked --offline --package fintwind --bin fintwind`: passed.
- Compiler acceptance: 17/17 cases passed. Repeated reports are byte-identical.
- Native SVG decoder: 8/8 cases passed (four sources, light/dark, two densities).
- Artifacts: `target/diagram-acceptance/compiler.json`,
  `target/diagram-acceptance/raster/report.json`, and the adjacent SVG samples.

These checks validate compilation, portable drawing attributes, theme-dependent
raster content, and higher-density output. They do not assert glyph containment,
interactive source toggling, copy behavior, or desktop scroll stability.

Eligible Mermaid sources up to 32 KiB remain one bounded media row in reasoning.
Pending, failed and explicitly selected source text above 2 KiB uses a retained,
height-limited native list whose text rows are at most 2 KiB. Full source remains
available to the header copy action. Mouse wheel, arrows, Page Up/Down and
Home/End operate that source viewport; it does not reshape all source per frame.
The source/diagram toggle appears only after a cached image is ready (or while
the user is explicitly viewing the source of a previously ready diagram).
Source viewport scroll/focus state is separate from its text parts: incremental
tail replacement updates the same native list, and live sources follow the tail
until the user scrolls away. Returning to End resumes live following.

Cache eviction protects explicit diagram request sets across frame boundaries,
not just transient `Arc` counts. If a raster cannot fit without evicting protected
results, admission caches a source fallback instead of repeatedly recompiling.
This pressure scenario has not been exercised in the desktop.
