//! Typed CLI adapter: parse first, then open only each command's resources.

mod commands;
mod dispatch;
mod general;
mod help;
mod input;
mod parse;
mod procedure;
mod share;
mod sync;

pub(crate) use commands::Command;
pub(crate) use dispatch::execute;
pub(crate) use parse::{parse, Parsed};
