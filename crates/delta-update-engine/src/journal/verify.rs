use std::path::Path;

use crate::crypto::{sha256_bytes, validate_public_key, verify_manifest};
use crate::error::EngineError;
use crate::journal::format::{FileOp, Journal};
use crate::journal::state::{self, ApplyConfig};
use crate::limits::Limits;
use crate::manifest::parse_manifest;
use crate::path::{has_parent_component, long_path, validate_install_path};

/// Verify that a journal is bound to the signed manifest that produced it.
///
/// A checksum alone is not authentication. The worker re-verifies the detached
/// minisign signature with its embedded public keys and rejects any `Place`
/// whose target hash is not in that manifest. Rollback targets are checked
/// against the installed manifest (or the pre-commit `old_hashes` snapshot for
/// the first update).
///
/// Empty or all-invalid key sets normally fail closed. `accept_unsigned`
/// exists only for tests and explicit legacy migrations; production callers
/// must pass `false`.
pub fn verify_commit_journal(
    journal_root: &Path,
    config: &ApplyConfig,
    journal: &Journal,
    keys: &[String],
    accept_unsigned: bool,
) -> Result<(), EngineError> {
    let valid_keys: Vec<String> = keys
        .iter()
        .filter(|key| validate_public_key(key).is_ok())
        .cloned()
        .collect();
    if valid_keys.is_empty() {
        if accept_unsigned {
            tracing::warn!(
                "journal signature verification skipped: allow_unsigned_journal_recovery is enabled"
            );
            return Ok(());
        }
        return Err(EngineError::JournalCorrupt(
            "no valid signing_public_keys; refusing to execute an unsigned journal".into(),
        ));
    }

    let Some(meta) = state::read_commit_meta(journal_root)? else {
        return Err(EngineError::JournalCorrupt(
            "commit.meta.json is missing; refusing an unauthenticated journal".into(),
        ));
    };
    if meta.commit_id != journal.commit_id {
        return Err(EngineError::JournalCorrupt(
            "commit metadata does not match the journal commit_id".into(),
        ));
    }
    // Compare resolved spellings: a journal written under an 8.3 short name or
    // a junction must still bind to the same install root after an upgrade.
    // `..` cannot be resolved lexically without lying about symlinks, so name it
    // instead of reporting a generic mismatch.
    if has_parent_component(&meta.install_root) || has_parent_component(&config.install_root) {
        return Err(EngineError::JournalCorrupt(
            "install_root contains '..'; refusing to compare an unresolved path".into(),
        ));
    }
    if long_path(&meta.install_root) != long_path(&config.install_root) {
        return Err(EngineError::JournalCorrupt(
            "commit metadata install_root does not match apply.json".into(),
        ));
    }
    let forward = journal.version == meta.version && journal.from_version == meta.from_version;
    let rollback = journal.version == meta.from_version && journal.from_version == meta.version;
    if !forward && !rollback {
        return Err(EngineError::JournalCorrupt(
            "journal version does not match the signed commit metadata".into(),
        ));
    }

    let Some((raw, signature)) = state::read_verified_manifest(journal_root)? else {
        return Err(EngineError::JournalCorrupt(
            "verified manifest is missing; refusing an unauthenticated journal".into(),
        ));
    };
    verify_manifest(&raw, &signature, &valid_keys).map_err(|error| {
        EngineError::JournalCorrupt(format!("signed manifest verification failed: {error}"))
    })?;
    if sha256_bytes(&raw) != meta.manifest_hash {
        return Err(EngineError::JournalCorrupt(
            "signed manifest hash does not match commit metadata".into(),
        ));
    }
    let manifest = parse_manifest(&raw, &Limits::default())?;

    let old_manifest = match state::read_installed_manifest(journal_root)? {
        Some((raw, signature)) => {
            verify_manifest(&raw, &signature, &valid_keys).map_err(|error| {
                EngineError::JournalCorrupt(format!(
                    "installed manifest verification failed: {error}"
                ))
            })?;
            Some(parse_manifest(&raw, &Limits::default())?)
        }
        None => None,
    };
    let old_hashes = state::read_old_hashes(journal_root)?;

    for op in &journal.ops {
        match op {
            FileOp::Place {
                dst, sha256, size, ..
            } => {
                validate_journal_path(dst)?;
                let entry = manifest.files.get(dst).ok_or_else(|| {
                    EngineError::JournalCorrupt(format!(
                        "Place target is not in the signed manifest: {dst}"
                    ))
                })?;
                if entry.sha256 != *sha256 || entry.size != *size {
                    return Err(EngineError::JournalCorrupt(format!(
                        "Place target hash/size does not match the signed manifest: {dst}"
                    )));
                }
            }
            FileOp::Restore {
                dst, sha256, size, ..
            } => {
                validate_journal_path(dst)?;
                let expected = old_manifest
                    .as_ref()
                    .and_then(|manifest| manifest.files.get(dst))
                    .map(|entry| (entry.sha256.as_str(), entry.size))
                    .or_else(|| {
                        old_hashes
                            .get(dst)
                            .map(|entry| (entry.sha256.as_str(), entry.size))
                    });
                let Some((expected_sha, expected_size)) = expected else {
                    return Err(EngineError::JournalCorrupt(format!(
                        "Restore target has no trusted old hash: {dst}"
                    )));
                };
                if expected_sha != sha256 || expected_size != *size {
                    return Err(EngineError::JournalCorrupt(format!(
                        "Restore target does not match the trusted old hash: {dst}"
                    )));
                }
            }
            FileOp::Backup { src, dst } => {
                validate_journal_path(src)?;
                let expected = format!(".old/{}/{}", journal.commit_id, src);
                if *dst != expected {
                    return Err(EngineError::JournalCorrupt(format!(
                        "Backup destination is not bound to the commit: {dst}"
                    )));
                }
            }
            FileOp::Trash { src, trash_dst } => {
                validate_journal_path(src)?;
                if trash_dst != src {
                    return Err(EngineError::JournalCorrupt(format!(
                        "Trash destination is not bound to its source: {trash_dst}"
                    )));
                }
            }
            FileOp::RemoveTrash { .. } => {}
        }
    }
    Ok(())
}

fn validate_journal_path(path: &str) -> Result<(), EngineError> {
    validate_install_path(path).map_err(|error| {
        EngineError::JournalCorrupt(format!("journal path is not a valid install path: {error}"))
    })?;
    Ok(())
}
