use std::io::Cursor;
use std::path::Path;

use anyhow::Context;

pub struct DetachedSignature {
    pub bytes: Vec<u8>,
}

/// Load a minisign secret key from a file. The password, when needed, comes
/// from `MINISIGN_PASSWORD` so it never appears in the process arguments.
pub fn load_secret_key(key_path: &Path) -> anyhow::Result<minisign::SecretKey> {
    let password = std::env::var("MINISIGN_PASSWORD").unwrap_or_default();
    minisign::SecretKey::from_file(key_path, Some(password))
        .with_context(|| format!("load secret key {}", key_path.display()))
}

/// Sign the raw manifest bytes. The signature covers the bytes on disk, not a
/// re-serialized JSON value.
pub fn sign_manifest(manifest: &[u8], key_path: &Path) -> anyhow::Result<DetachedSignature> {
    let secret_key = load_secret_key(key_path)?;
    let signature = minisign::sign(
        None,
        &secret_key,
        Cursor::new(manifest),
        None,
        Some("delta-update manifest"),
    )
    .context("sign manifest")?;
    Ok(DetachedSignature {
        bytes: signature.to_bytes(),
    })
}

pub fn public_key_base64(key_path: &Path) -> anyhow::Result<String> {
    let secret_key = load_secret_key(key_path)?;
    let public_key = minisign::PublicKey::from_secret_key(&secret_key)
        .context("derive public key from secret key")?;
    Ok(public_key.to_base64())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_key(dir: &Path) -> std::path::PathBuf {
        let keypair = minisign::KeyPair::generate_encrypted_keypair(Some(String::new())).unwrap();
        let key_path = dir.join("test.key");
        std::fs::write(&key_path, keypair.sk.to_box(None).unwrap().to_bytes()).unwrap();
        key_path
    }

    #[test]
    fn signature_verifies_against_the_derived_public_key() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = write_key(dir.path());
        let manifest = br#"{"schema":1}"#;
        let signature = sign_manifest(manifest, &key_path).unwrap();
        let public_key = public_key_base64(&key_path).unwrap();
        delta_update_engine::crypto::verify_manifest(
            manifest,
            std::str::from_utf8(&signature.bytes).unwrap(),
            &[public_key],
        )
        .unwrap();
    }
}
