//! Real compiler + native SVG decoder acceptance, without a window or service.
//! Failure matrix: docs/diagram-native-acceptance.md. No user processes are used.

use std::{borrow::Cow, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, ensure};
use fintwind_diagram::diagram_compile::{Theme, compile};
use gpui::{AssetSource, SharedString, SvgRenderer};
use serde_json::json;

struct Fonts;

const FONTS: &[(&str, &[u8])] = &[
    (
        "fonts/JetBrainsMono-Regular.ttf",
        include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    ),
    (
        "fonts/JetBrainsMono-Bold.ttf",
        include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    ),
    (
        "fonts/JetBrainsMono-Italic.ttf",
        include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
    ),
    (
        "fonts/JetBrainsMono-BoldItalic.ttf",
        include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
    ),
];

impl AssetSource for Fonts {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(FONTS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, data)| Cow::Borrowed(*data)))
    }

    fn list(&self, prefix: &str) -> Result<Vec<SharedString>> {
        Ok(FONTS
            .iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

fn main() -> Result<()> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("expected an artifact directory")?,
    );
    std::fs::create_dir_all(&output)?;
    // A failed rerun must not leave an older successful report behind.
    std::fs::write(
        output.join("report.json"),
        br#"{"passed":false,"stage":"running"}"#,
    )?;
    let renderer = SvgRenderer::new(Arc::new(Fonts));
    let themes = [
        Theme {
            background: [250, 250, 250],
            foreground: [25, 25, 25],
            muted: [100, 100, 100],
            border: [130, 130, 130],
            accent: [40, 90, 190],
        },
        Theme {
            background: [25, 25, 25],
            foreground: [240, 240, 240],
            muted: [170, 170, 170],
            border: [120, 120, 120],
            accent: [100, 150, 250],
        },
    ];
    let sources = [
        ("flowchart", "flowchart LR\n A[Request] --> B[Response]"),
        (
            "sequence",
            "sequenceDiagram\n participant Client\n participant Server\n Client->>Server: Request\n Server-->>Client: Response",
        ),
        (
            "class",
            "classDiagram\n class Account {\n +String name\n +save()\n }",
        ),
        (
            "cjk",
            "flowchart LR\n A[\u{8bf7}\u{6c42}] --> B[\u{54cd}\u{5e94}]",
        ),
    ];
    let mut cases = Vec::new();
    for (name, source) in sources {
        let mut previous = None;
        for (index, theme) in themes.iter().enumerate() {
            let diagram = compile(source, *theme, 1_735_689_600_000.0)
                .with_context(|| format!("compile {name}"))?;
            let image = renderer
                .render_single_frame(diagram.svg.as_bytes(), 1.0)
                .with_context(|| format!("rasterize {name}"))?;
            let bytes = image.as_bytes(0).context("missing raster frame")?;
            let ink = bytes
                .chunks_exact(4)
                .filter(|pixel| {
                    pixel[3] > 0
                        && [pixel[2], pixel[1], pixel[0]]
                            .iter()
                            .zip(theme.background)
                            .any(|(channel, background)| channel.abs_diff(background) > 20)
                })
                .count();
            ensure!(ink > 50, "{name}: raster has no meaningful strokes or text");
            if let Some(previous) = previous.as_ref() {
                ensure!(
                    previous != &bytes.to_vec(),
                    "{name}: theme did not change the raster"
                );
            }
            previous = Some(bytes.to_vec());
            let high_dpi = renderer.render_single_frame(diagram.svg.as_bytes(), 2.0)?;
            ensure!(
                high_dpi
                    .as_bytes(0)
                    .context("missing high-DPI frame")?
                    .len()
                    > bytes.len() * 3,
                "{name}: high-DPI raster did not increase resolution"
            );
            std::fs::write(output.join(format!("{name}-{index}.svg")), &diagram.svg)?;
            cases.push(json!({"name": name, "theme": index, "width": diagram.width,
                "height": diagram.height, "raster_bytes": bytes.len(), "ink_pixels": ink,
                "high_dpi_bytes": high_dpi.as_bytes(0).map(|bytes| bytes.len()), "passed": true}));
        }
    }
    std::fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&json!({
            "ondine_revision": "439fc64aefdb06e3451fac6c44266e5965ea7265", "passed": true, "cases": cases,
            "scope": "native compiler and SVG decoder; no desktop window or visual acceptance"
        }))?,
    )?;
    println!("[OK] {}", output.join("report.json").display());
    Ok(())
}
