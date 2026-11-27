//! Markdown parsing, syntax highlighting, LaTeX math, Mermaid diagrams, and mention/privacy rendering.

macro_rules! tr {
    ($($args:tt)*) => {
        fintwind_protocol::tr!($($args)*)
    };
}

pub mod file_target;
pub mod md;
pub mod mentions;
pub mod prompt_privacy;
