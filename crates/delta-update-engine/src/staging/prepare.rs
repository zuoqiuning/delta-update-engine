use std::io::Read;
use std::path::Path;
use std::sync::Mutex;

use futures_util::{StreamExt, TryStreamExt};
use tokio_util::sync::CancellationToken;

use crate::crypto::{sha256_bytes, sha256_file};
use crate::engine::Progress;
use crate::error::{EngineError, ErrorCategory, PatchError};
use crate::fetch::Fetcher;
use crate::limits::Limits;
use crate::manifest::diff::{DiffResult, ModifiedFile};
use crate::manifest::model::{FileEntry, Manifest, Strategy};
use crate::patch::apply::apply_patch;
use crate::patch::container::{PatchHeader, PATCH_HEADER_LEN};

use super::plan::{PreparedPlan, StagedFile};

const MAX_RETRIES: u32 = 2;

pub(crate) struct PrepareInput<'a> {
    pub manifest: &'a Manifest,
    pub diff: &'a DiffResult,
    pub fetcher: &'a dyn Fetcher,
    pub install_root: &'a Path,
    pub staging_root: &'a Path,
    pub version: &'a str,
    pub manifest_hash: &'a str,
    pub limits: &'a Limits,
    pub cancel: &'a CancellationToken,
    pub progress: &'a (dyn Fn(Progress) + Send + Sync),
}

struct StageContext<'a> {
    fetcher: &'a dyn Fetcher,
    files_root: &'a Path,
    patches_root: &'a Path,
    limits: &'a Limits,
    cancel: &'a CancellationToken,
    progress: &'a (dyn Fn(Progress) + Send + Sync),
    total_download: u64,
    downloaded: &'a Mutex<u64>,
}

/// Download and materialize changed files, then verify the whole target set.
///
/// Unchanged files are verified in place and never copied into staging.
pub(crate) async fn prepare(input: &PrepareInput<'_>) -> Result<PreparedPlan, EngineError> {
    let target_root = input.staging_root.join(input.version);
    let files_root = target_root.join("files");
    let patches_root = target_root.join("patches");
    std::fs::create_dir_all(&files_root).map_err(|err| io_error(err, "staging"))?;
    std::fs::create_dir_all(&patches_root).map_err(|err| io_error(err, "staging"))?;

    preflight(input.manifest, input.diff, input.staging_root, input.limits)?;
    verify_unchanged(input).await?;

    let downloaded = Mutex::new(0);
    let context = StageContext {
        fetcher: input.fetcher,
        files_root: &files_root,
        patches_root: &patches_root,
        limits: input.limits,
        cancel: input.cancel,
        progress: input.progress,
        total_download: input.diff.download_bytes,
        downloaded: &downloaded,
    };

    // Own the work items: async blocks over `&ModifiedFile`/`&String` make the
    // returned future non-general over lifetimes, which breaks Tauri's command
    // macros downstream.
    let limit = input.limits.max_concurrent_downloads.max(1);
    let modified = input.diff.modified.clone();
    let added: Vec<(String, FileEntry)> = input
        .diff
        .added
        .iter()
        .map(|path| {
            let entry = input.manifest.files.get(path).cloned().ok_or_else(|| {
                EngineError::ManifestInvalid(format!(
                    "{path}: added entry is missing from the manifest"
                ))
            })?;
            Ok((path.clone(), entry))
        })
        .collect::<Result<_, EngineError>>()?;

    let mut staged: Vec<StagedFile> = futures_util::stream::iter(modified)
        .map(|file| {
            let context = &context;
            async move {
                check_cancel(input.cancel)?;
                let source = input.install_root.join(&file.path);
                prepare_modified(&file, &source, context).await
            }
        })
        .buffer_unordered(limit)
        .try_collect()
        .await?;

    let added: Vec<StagedFile> = futures_util::stream::iter(added)
        .map(|(path, entry)| {
            let context = &context;
            async move {
                check_cancel(input.cancel)?;
                stage_full(&path, &entry, context).await
            }
        })
        .buffer_unordered(limit)
        .try_collect()
        .await?;
    staged.extend(added);

    let mut all_files: Vec<String> = input.manifest.files.keys().cloned().collect();
    all_files.sort();
    staged.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(PreparedPlan {
        version: input.version.to_string(),
        manifest_hash: input.manifest_hash.to_string(),
        staging_root: target_root,
        files: staged,
        removed: input.diff.removed.clone(),
        all_files,
    })
}

fn preflight(
    manifest: &Manifest,
    diff: &DiffResult,
    staging_root: &Path,
    limits: &Limits,
) -> Result<(), EngineError> {
    let changed: Vec<&FileEntry> = diff
        .modified
        .iter()
        .map(|file| &file.entry)
        .chain(
            diff.added
                .iter()
                .filter_map(|path| manifest.files.get(path)),
        )
        .collect();
    let materialized = changed
        .iter()
        .fold(0_u64, |total, entry| total.saturating_add(entry.size));
    let worst_download = changed.iter().fold(0_u64, |total, entry| {
        total.saturating_add(entry.size.max(entry.delta_size.unwrap_or(0)))
    });
    let required = materialized.saturating_add(worst_download);
    if required > limits.staging_size_limit {
        return Err(EngineError::StagingLimit {
            limit: limits.staging_size_limit,
        });
    }
    let available = fs4::available_space(staging_root).map_err(|err| io_error(err, "staging"))?;
    if available < required {
        return Err(EngineError::InsufficientDiskSpace {
            required,
            available,
        });
    }
    Ok(())
}

/// Re-hash every unchanged file in place. The install tree can be large, so
/// the scan runs on the blocking pool instead of stalling the async runtime.
async fn verify_unchanged(input: &PrepareInput<'_>) -> Result<(), EngineError> {
    let unchanged: Vec<(String, String)> = input
        .diff
        .unchanged
        .iter()
        .map(|path| {
            input
                .manifest
                .files
                .get(path)
                .map(|entry| (path.clone(), entry.sha256.clone()))
                .ok_or_else(|| {
                    EngineError::ManifestInvalid(format!("{path}: unchanged entry is missing"))
                })
        })
        .collect::<Result<_, _>>()?;
    let install_root = input.install_root.to_path_buf();
    let cancel = input.cancel.clone();
    tokio::task::spawn_blocking(move || verify_unchanged_files(&unchanged, &install_root, &cancel))
        .await
        .map_err(|err| EngineError::Internal(format!("verify task: {err}")))?
}

fn verify_unchanged_files(
    unchanged: &[(String, String)],
    install_root: &Path,
    cancel: &CancellationToken,
) -> Result<(), EngineError> {
    for (path, expected) in unchanged {
        check_cancel(cancel)?;
        let actual = sha256_file(&install_root.join(path)).map_err(|err| io_error(err, path))?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(PatchError::SourceMismatch(format!(
                "{path}: local file changed after check"
            ))
            .into());
        }
    }
    Ok(())
}

async fn prepare_modified(
    file: &ModifiedFile,
    source: &Path,
    context: &StageContext<'_>,
) -> Result<StagedFile, EngineError> {
    let path = &file.path;
    let entry = &file.entry;
    let dest = context.files_root.join(path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| io_error(err, path))?;
    }

    if should_try_patch(entry, context.limits) {
        (context.progress)(Progress::Applying { file: path.clone() });
        match stage_patch(path, entry, source, &dest, context).await {
            Ok(staged) => {
                (context.progress)(Progress::Verifying { file: path.clone() });
                return Ok(staged);
            }
            Err(error) if fallback_allowed(&error) => {
                tracing::warn!(path, %error, "patch failed; falling back to the full object");
                let _ = std::fs::remove_file(&dest);
            }
            Err(error) => return Err(error),
        }
    }

    stage_full(path, entry, context).await
}

async fn stage_patch(
    path: &str,
    entry: &FileEntry,
    source: &Path,
    dest: &Path,
    context: &StageContext<'_>,
) -> Result<StagedFile, EngineError> {
    let source_size = std::fs::metadata(source)
        .map_err(|err| io_error(err, path))?
        .len();
    if source_size > context.limits.max_patch_file_size
        || entry.size > context.limits.max_patch_file_size
    {
        return Err(PatchError::MemoryBudget {
            file_size: source_size.max(entry.size),
            budget: context.limits.max_patch_file_size,
        }
        .into());
    }

    let delta_key = entry
        .delta_key
        .as_deref()
        .ok_or_else(|| PatchError::FormatMismatch(format!("{path}: missing delta_key")))?;
    let delta_size = entry
        .delta_size
        .ok_or_else(|| PatchError::FormatMismatch(format!("{path}: missing delta_size")))?;
    let patch_path = context
        .patches_root
        .join(format!("{}.tdp", sha256_bytes(path.as_bytes())));
    download_verified(
        context.fetcher,
        delta_key,
        &patch_path,
        delta_size,
        None,
        context.cancel,
    )
    .await?;
    report_download(context, path, delta_size);

    // Read only the fixed-size header; the payload is parsed by `apply_patch`.
    // Loading the whole patch here would double peak memory for large files.
    let mut header_bytes = [0_u8; PATCH_HEADER_LEN];
    let mut patch_file = std::fs::File::open(&patch_path).map_err(|err| io_error(err, path))?;
    patch_file
        .read_exact(&mut header_bytes)
        .map_err(|_| PatchError::FormatMismatch(format!("{path}: patch is truncated")))?;
    drop(patch_file);
    let (header, _) = PatchHeader::parse(&header_bytes)?;
    validate_header(entry, &header, source, path)?;

    let source_owned = source.to_path_buf();
    let patch_owned = patch_path.clone();
    let dest_owned = dest.to_path_buf();
    let limits_owned = context.limits.clone();
    tokio::task::spawn_blocking(move || {
        apply_patch(
            &source_owned,
            &patch_owned,
            &dest_owned,
            &header,
            &limits_owned,
        )
    })
    .await
    .map_err(|err| EngineError::Internal(format!("patch task: {err}")))??;
    let _ = std::fs::remove_file(&patch_path);

    verify_staged(path, dest, entry).await?;
    Ok(StagedFile {
        path: path.to_string(),
        staging_path: dest.to_path_buf(),
        sha256: entry.sha256.clone(),
        size: entry.size,
    })
}

fn validate_header(
    entry: &FileEntry,
    header: &PatchHeader,
    source: &Path,
    path: &str,
) -> Result<(), EngineError> {
    let expected_id = entry
        .strategy
        .patch_id()
        .ok_or_else(|| PatchError::FormatMismatch(format!("{path}: strategy has no patch id")))?;
    if header.strategy_id != expected_id {
        return Err(PatchError::FormatMismatch(format!(
            "{path}: container strategy {} does not match manifest {}",
            header.strategy_id, expected_id
        ))
        .into());
    }
    if header.target_size != entry.size {
        return Err(PatchError::FormatMismatch(format!(
            "{path}: container target size {} does not match manifest {}",
            header.target_size, entry.size
        ))
        .into());
    }
    if hex_to_32(&entry.sha256) != Some(header.target_sha256) {
        return Err(PatchError::FormatMismatch(format!(
            "{path}: container target hash does not match manifest"
        ))
        .into());
    }
    let source_hash = sha256_file(source).map_err(|err| io_error(err, path))?;
    if hex_to_32(&source_hash) != Some(header.source_sha256) {
        return Err(PatchError::SourceMismatch(format!(
            "{path}: local source hash does not match the patch"
        ))
        .into());
    }
    Ok(())
}

/// Report cumulative download bytes. The total is the planned download size;
/// a patch fallback may push the counter past it, so the value is clamped.
fn report_download(context: &StageContext<'_>, file: &str, bytes: u64) {
    // Hold the lock across the callback so concurrent downloads cannot emit
    // progress out of order (the UI would otherwise jump backwards).
    let mut completed = context
        .downloaded
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *completed = (*completed)
        .saturating_add(bytes)
        .min(context.total_download);
    (context.progress)(Progress::Downloading {
        file: file.to_string(),
        total: context.total_download,
        completed: *completed,
    });
}

async fn stage_full(
    path: &str,
    entry: &FileEntry,
    context: &StageContext<'_>,
) -> Result<StagedFile, EngineError> {
    let dest = context.files_root.join(path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| io_error(err, path))?;
    }
    let key = entry
        .full_key
        .as_deref()
        .ok_or_else(|| PatchError::UnknownStrategy(format!("{path}: no full_key")))?;
    download_verified(
        context.fetcher,
        key,
        &dest,
        entry.size,
        Some(&entry.sha256),
        context.cancel,
    )
    .await?;
    report_download(context, path, entry.size);
    (context.progress)(Progress::Verifying {
        file: path.to_string(),
    });
    Ok(StagedFile {
        path: path.to_string(),
        staging_path: dest,
        sha256: entry.sha256.clone(),
        size: entry.size,
    })
}

async fn download_verified(
    fetcher: &dyn Fetcher,
    key: &str,
    dest: &Path,
    expected_size: u64,
    expected_sha256: Option<&str>,
    cancel: &CancellationToken,
) -> Result<(), EngineError> {
    if let Some(expected) = expected_sha256 {
        if tokio::fs::metadata(dest)
            .await
            .is_ok_and(|meta| meta.len() == expected_size)
        {
            let actual = hash_file(dest).await.map_err(|err| io_error(err, key))?;
            if actual == expected {
                return Ok(());
            }
        }
    }

    for attempt in 0..=MAX_RETRIES {
        download_with_retry(fetcher, key, dest, expected_size, cancel).await?;
        let metadata = tokio::fs::metadata(dest)
            .await
            .map_err(|err| io_error(err, key))?;
        if metadata.len() == expected_size {
            if let Some(expected) = expected_sha256 {
                let actual = hash_file(dest).await.map_err(|err| io_error(err, key))?;
                if actual == expected {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }
        let _ = std::fs::remove_file(dest);
        if attempt == MAX_RETRIES {
            return Err(PatchError::TargetHashMismatch(format!(
                "{key}: downloaded object does not match the manifest"
            ))
            .into());
        }
    }
    unreachable!("retry loop always returns")
}

async fn download_with_retry(
    fetcher: &dyn Fetcher,
    key: &str,
    dest: &Path,
    max_bytes: u64,
    cancel: &CancellationToken,
) -> Result<(), EngineError> {
    let mut attempt = 0;
    loop {
        let mut resume = tokio::fs::metadata(dest)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        if resume > max_bytes {
            // A stale or oversized partial file cannot be resumed.
            let _ = std::fs::remove_file(dest);
            resume = 0;
        }
        let result = tokio::select! {
            _ = cancel.cancelled() => return Err(EngineError::Cancelled),
            result = fetcher.download(key, dest, resume, max_bytes) => result,
        };
        match result {
            Ok(_) => return Ok(()),
            Err(error) if error.retryable() && attempt < MAX_RETRIES => {
                attempt += 1;
                let delay = error
                    .retry_after()
                    .unwrap_or_else(|| std::time::Duration::from_millis(200_u64 << (attempt - 1)));
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn should_try_patch(entry: &FileEntry, limits: &Limits) -> bool {
    entry.strategy == Strategy::QbsdiffZstd
        && entry.delta_key.is_some()
        && entry
            .delta_size
            .is_some_and(|size| size <= limits.max_patch_file_size)
        && entry.size <= limits.max_patch_file_size
}

fn fallback_allowed(error: &EngineError) -> bool {
    matches!(
        error.category(),
        ErrorCategory::Patch | ErrorCategory::Network
    )
}

/// Hash a downloaded or staged file on the blocking pool.
async fn hash_file(path: &Path) -> std::io::Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(std::io::Error::other)?
}

async fn verify_staged(path: &str, dest: &Path, entry: &FileEntry) -> Result<(), EngineError> {
    let metadata = std::fs::metadata(dest).map_err(|err| io_error(err, path))?;
    if metadata.len() != entry.size {
        return Err(PatchError::TargetHashMismatch(format!(
            "{path}: staged size {} does not match {}",
            metadata.len(),
            entry.size
        ))
        .into());
    }
    let actual = hash_file(dest).await.map_err(|err| io_error(err, path))?;
    if actual != entry.sha256 {
        return Err(PatchError::TargetHashMismatch(format!(
            "{path}: staged hash does not match the manifest"
        ))
        .into());
    }
    Ok(())
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), EngineError> {
    if cancel.is_cancelled() {
        Err(EngineError::Cancelled)
    } else {
        Ok(())
    }
}

fn hex_to_32(value: &str) -> Option<[u8; 32]> {
    hex::decode(value).ok()?.try_into().ok()
}

fn io_error(err: std::io::Error, context: &str) -> EngineError {
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        EngineError::PermissionDenied(format!("{context}: {err}"))
    } else {
        EngineError::Internal(format!("{context}: {err}"))
    }
}
