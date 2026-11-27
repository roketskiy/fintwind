//! Provider quota adapters, local CLI subscription probes, and usage indexing.

macro_rules! tr {
    ($($args:tt)*) => {
        fintwind_protocol::tr!($($args)*)
    };
}

pub mod claude_pty;
pub mod local_quota;
pub mod provider_quota;
pub mod usage;
pub mod usage_index;
pub mod usage_scan;
