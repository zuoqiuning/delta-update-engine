//! Hashing and minisign verification.

pub mod hash;
pub mod minisign;

pub use hash::{sha256_bytes, sha256_file};
pub(crate) use minisign::is_key_id;
pub use minisign::{validate_public_key, verify_manifest};
