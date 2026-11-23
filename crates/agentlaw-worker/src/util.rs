//! Shared live I/O result type.
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
