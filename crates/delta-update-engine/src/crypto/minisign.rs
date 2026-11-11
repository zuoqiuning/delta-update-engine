use minisign_verify::{PublicKey, Signature};

use crate::error::{EngineError, SignatureError};

/// Verify a detached minisign signature against any trusted key.
///
/// `signature` is the full `.minisig` file text (untrusted comment, base64
/// payload, trusted comment). `keys` accept either a raw base64 payload or a
/// `.pub` file with a comment line.
pub fn verify_manifest(
    raw_manifest: &[u8],
    signature: &str,
    keys: &[String],
) -> Result<(), EngineError> {
    let signature = Signature::decode(signature).map_err(|err| {
        EngineError::Signature(SignatureError::Invalid(format!(
            "cannot parse detached signature: {err}"
        )))
    })?;

    let mut trusted_key_failed = false;
    for key in keys {
        let Ok(public_key) = PublicKey::from_base64(first_payload_line(key)) else {
            continue;
        };
        match public_key.verify(raw_manifest, &signature, true) {
            Ok(()) => return Ok(()),
            // The key ID matched, so the signature was produced by a trusted
            // key but does not match these bytes: the manifest was tampered.
            Err(minisign_verify::Error::InvalidSignature) => trusted_key_failed = true,
            Err(_) => {}
        }
    }

    if trusted_key_failed {
        Err(EngineError::Signature(SignatureError::Invalid(
            "signature does not match the manifest".into(),
        )))
    } else {
        Err(EngineError::Signature(SignatureError::NoTrustedKey))
    }
}

/// Validate a configured public key before any network request.
pub fn validate_public_key(value: &str) -> Result<(), EngineError> {
    PublicKey::from_base64(first_payload_line(value))
        .map(|_| ())
        .map_err(|err| EngineError::ConfigInvalid(format!("invalid minisign public key: {err}")))
}

/// Accept either a raw base64 payload or a minisign file with comment lines.
fn first_payload_line(value: &str) -> &str {
    value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
        .unwrap_or("")
}
