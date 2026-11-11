//! Installation configuration, setup, update gate and result delivery.

pub mod config;
pub mod delivery;
pub mod gate;
pub mod machine;
pub mod owner;
pub mod registry;
pub mod setup;

pub use crate::protocol::BindingError;

pub fn build_identity(executable: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(executable)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}
