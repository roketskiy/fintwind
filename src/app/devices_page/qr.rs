//! The pairing QR, painted as quads instead of thousands of divs.
//!
//! A pair URL is a few hundred bytes, which is a version-13-plus QR: some
//! five thousand modules, about half of them dark. One div per dark module
//! hands the layout engine thousands of nodes on every frame the pairing
//! card is visible. Instead the module matrix is computed once (off the UI
//! thread, when the invite lands), merged into horizontal dark runs once,
//! and painted as one quad per run — a few hundred quads, no element
//! children, and no per-frame allocation.
//!
//! The symbol is deliberately theme-independent: scanners need a dark
//! symbol on a light plate, so both themes get the same white plate.

use std::sync::Arc;

use gpui::{
    App, BorderStyle, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Style, Window, black, point, px, quad, size, white,
};

/// One module's edge. Large enough for a phone camera at arm's length,
/// small enough that a version-13 symbol stays inside the settings card.
const MODULE_SIZE: f32 = 4.0;

/// Quiet zone in modules. The spec asks for four; three on a white plate
/// that fills the element keeps the symbol compact without losing scans.
const QUIET_MODULES: f32 = 3.0;

/// The encoded symbol: module grid plus the dark runs to paint.
pub struct QrMatrix {
    modules: usize,
    /// `(row, start_column, length)` per horizontal dark run, built once.
    runs: Vec<(u32, u32, u32)>,
}

impl QrMatrix {
    /// Encode `payload` at error-correction level M. Runs are merged here,
    /// on the caller's thread, so painting never walks the module grid.
    pub fn new(payload: &str) -> anyhow::Result<Self> {
        let code =
            qrcode::QrCode::with_error_correction_level(payload.as_bytes(), qrcode::EcLevel::M)?;
        let width = code.width();
        let colors = code.to_colors();
        let mut runs = Vec::new();
        for row in 0..width {
            let mut run_start: Option<usize> = None;
            for column in 0..width {
                let dark = colors[row * width + column] == qrcode::Color::Dark;
                match (dark, run_start) {
                    (true, None) => run_start = Some(column),
                    (false, Some(start)) => {
                        runs.push((row as u32, start as u32, (column - start) as u32));
                        run_start = None;
                    }
                    _ => {}
                }
            }
            if let Some(start) = run_start {
                runs.push((row as u32, start as u32, (width - start) as u32));
            }
        }
        Ok(Self {
            modules: width,
            runs,
        })
    }

    /// Modules per side, quiet zone excluded.
    pub fn module_count(&self) -> usize {
        self.modules
    }
}

/// One fixed-size element that paints a [`QrMatrix`]. Stateless across
/// frames: layout is a constant, prepaint does nothing, paint draws the
/// plate and the pre-merged runs.
pub struct QrElement {
    matrix: Arc<QrMatrix>,
}

impl QrElement {
    pub fn new(matrix: Arc<QrMatrix>) -> Self {
        Self { matrix }
    }

    /// The element's side: the symbol plus its quiet zone on both axes.
    fn side(&self) -> Pixels {
        px(MODULE_SIZE * (self.matrix.module_count() as f32 + QUIET_MODULES * 2.0))
    }
}

impl Element for QrElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::Name("devices-pairing-qr".into()))
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let side = self.side();
        let layout_id = window.request_layout(
            Style {
                size: size(side.into(), side.into()),
                ..Style::default()
            },
            [],
            cx,
        );
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) {
        // The plate: white across the whole element, quiet zone included.
        window.paint_quad(quad(
            bounds,
            px(0.0),
            white(),
            px(0.0),
            gpui::transparent_black(),
            BorderStyle::default(),
        ));
        let module = px(MODULE_SIZE);
        let origin = bounds.origin + point(module * QUIET_MODULES, module * QUIET_MODULES);
        for &(row, start, length) in &self.matrix.runs {
            let x = origin.x + module * start as f32;
            let y = origin.y + module * row as f32;
            let width = module * length as f32;
            window.paint_quad(quad(
                Bounds::new(point(x, y), size(width, module)),
                px(0.0),
                black(),
                px(0.0),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        }
    }
}

impl IntoElement for QrElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
