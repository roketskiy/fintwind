//! GPUI multi-line composer input and single-line text field.

macro_rules! tr {
    ($($args:tt)*) => {
        fintwind_protocol::tr!($($args)*)
    };
}

pub mod input;
pub mod text_field;
#[cfg(test)]
mod input_hit_test_e2e;

pub use input::*;
