//! Calls the code doesn't spell out, for cargo-atlas's tests.

pub mod config;
pub mod domain;
pub mod guard;
pub mod log;
pub mod money;
pub mod net;
pub mod writers;

#[path = "extra/renamed.rs"]
pub mod renamed;
