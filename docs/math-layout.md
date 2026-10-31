# Native formula layout

Fintwind keeps RaTeX parsing, layout, SVG ink measurement and rasterization on
bounded background workers. Rendering only reads cached results.

## Readability and geometry

- Inline math uses 1.21 times the surrounding text's em size (KaTeX's default
  optical compensation); display math uses 1.25 times. Both follow text-size
  settings. TeX still controls the relative sizes of scripts and large operators.
- Mixed rows share the shaped text's alphabetic baseline. Formula ascent and
  descent expand the row as needed; short rows receive symmetric leading instead
  of a fabricated 80/20 ascent/descent split.
- SVG framing preserves the TeX layout box and expands it to include outlined
  ink and one logical pixel of padding. Baseline coordinates move with the
  viewport origin, not by scaling glyphs. Ink measurement uses the same usvg
  version as GPUI and runs only on workers. It is inspired by Ely's formula
  framing: <https://docs.rs/ely-documents/latest/src/ely_documents/math.rs.html>.
- Source-size, item-count, texture-size, raster-pixel and cache limits remain
  bounded. An unsupported or oversized formula retains the source fallback.

## Overflow

An overwide formula occupies its own row at natural size. Only that formula
scrolls; surrounding prose keeps its wrapping and position. Its small, permanent
scroll track needs no animation timer.

Use horizontal wheel/trackpad input, Shift+wheel, or click/drag the scroll track.
Focus the math paragraph and use Left/Right or Home/End. Up/Down chooses between
multiple overflowing formulas in the same paragraph. Ordinary vertical wheel
input remains available to the enclosing transcript. Selection and expression
copying retain the original LaTeX source while scrolled.

## Nonvisual verification

```sh
cargo check --locked --offline -p fintwind
python scripts/math-layout-e2e.py --timeout 1200
```

The runner uses a fresh data directory, an environment whitelist and a Windows
Job Object containing only its own process tree. It starts no real application,
daemon or provider. Artifacts are `proof.json` and `test.log` in the printed
`target/math-layout-e2e/<run>/` directory. These checks do not replace checking
the visible result in the signed development app when visual testing is requested.
