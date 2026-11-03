//! Compile the actual vendored estimator and its private regressions with the
//! same public core types. The vendored crate is excluded from the workspace.
pub use i_slint_core::Coord;
mod animations {
    pub use i_slint_core::animations::Instant;
}
mod lengths {
    pub use i_slint_core::lengths::{LogicalPx, LogicalVector};
}
#[rustfmt::skip]
#[path = "../vendor/i-slint-core/items/flickable/data_ringbuffer.rs"]
mod data_ringbuffer;
