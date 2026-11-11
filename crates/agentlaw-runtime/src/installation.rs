//! Installation configuration, setup, update gate and result delivery.

pub mod config;
pub mod delivery;
pub mod gate;
pub mod machine;
pub mod owner;
pub mod registry;
pub mod setup;

pub use crate::protocol::BindingError;
