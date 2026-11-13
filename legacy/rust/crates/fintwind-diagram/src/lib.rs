//! Mermaid compilation for Fintwind.
//!
//! The app's markdown renderer rasterizes diagrams, and the daemon's
//! `fintwind_mermaid_check` tool validates the same sources for the model.
//! Both call the one compiler in [`diagram_compile`], so a check can never
//! pass on a source the renderer would reject, or report an error the
//! renderer would not show.

pub mod diagram_compile;
