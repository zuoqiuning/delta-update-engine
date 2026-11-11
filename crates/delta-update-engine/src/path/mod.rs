//! Relative path validation and install-root resolution.

pub mod guard;
pub mod rel_path;
pub mod resolver;

pub use guard::{resolve_within, validate_rel_path};
pub use rel_path::RelPath;
pub use resolver::resolve_install_root;
