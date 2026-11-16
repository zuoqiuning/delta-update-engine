//! Relative path validation and install-root resolution.

pub mod guard;
pub mod rel_path;
pub mod resolver;

pub use guard::{resolve_within, validate_install_path, validate_rel_path};
pub use rel_path::RelPath;
pub use resolver::{has_parent_component, install_form, long_path, resolve_install_root};
