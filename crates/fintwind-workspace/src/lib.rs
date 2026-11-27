//! Daemon-owned workspace operations, skills, composer discovery, and PTYs.

macro_rules! tr {
    ($($args:tt)*) => {
        fintwind_protocol::tr!($($args)*)
    };
}

pub mod composer_complete;
pub mod projectless;
pub mod skills;
pub mod terminal;
pub mod workspace;
