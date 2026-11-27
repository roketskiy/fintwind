//! GPUI integration settings pages: curated MCP Market and Skills Library.

macro_rules! tr {
    ($key:expr) => {
        fintwind_protocol::tr!($key)
    };
    ($key:expr, $($args:tt)*) => {
        fintwind_protocol::tr!($key, $($args)*)
    };
}

pub(crate) use fintwind_protocol::i18n;

pub mod mcp_market;
pub mod skills;

pub use mcp_market::{McpMarketHost, McpMarketPage};
pub use skills::{SkillsHost, SkillsPage};
