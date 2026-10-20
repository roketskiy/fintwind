pub mod cache;
mod diagram;
pub mod highlight;
pub mod math;
pub mod mend;
pub mod parser;
pub mod render;
pub mod selection;
pub mod veil;
pub mod virtualized;

/// Both Markdown paths must agree on which fences remain a single media row.
fn diagram_fence(language: Option<&str>, source: &str) -> bool {
    language.is_some_and(|tag| tag.eq_ignore_ascii_case("mermaid"))
        && source.len() <= fintwind_diagram::diagram_compile::MAX_SOURCE_BYTES
}
