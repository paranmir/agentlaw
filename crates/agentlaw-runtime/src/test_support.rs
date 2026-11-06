//! Fixtures shared by runtime tests: an isolated installation and fake RAM ports.

pub mod files;
pub mod index_work;
pub mod installation;
pub mod semantic;

pub use files::*;
pub use index_work::*;
pub use installation::*;
pub use semantic::*;
