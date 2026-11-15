use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

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

/// A patch container this client can apply.
///
/// Adding a strategy means adding an implementation and one entry in
/// [`APPLIERS`]; `prepare_modified` and `stage_patch` stay unchanged.
trait PatchApplier: Sync {
    fn strategy(&self) -> Strategy;

    /// Parse the fixed-size header and bind it to the signed manifest entry.
    fn parse_header(
        &self,
        bytes: &[u8],
        entry: &FileEntry,
        path: &str,
    ) -> Result<PatchHeader, EngineError>;

    /// Apply the patch payload to `source`, writing the result to `dest`.
    fn apply(
        &self,
        source: &Path,
        patch: &Path,
        dest: &Path,
        header: &PatchHeader,
        limits: &Limits,
    ) -> Result<(), EngineError>;
}

struct QbsdiffApplier;

impl PatchApplier for QbsdiffApplier {
    fn strategy(&self) -> Strategy {
        Strategy::QbsdiffZstd
    }

    fn parse_header(
        &self,
        bytes: &[u8],
        entry: &FileEntry,
        path: &str,
    ) -> Result<PatchHeader, EngineError> {
        let (header, _) = PatchHeader::parse(bytes)?;
        validate_header(entry, &header, path)?;
        Ok(header)
    }

    fn apply(
        &self,
        source: &Path,
        patch: &Path,
        dest: &Path,
        header: &PatchHeader,
        limits: &Limits,
    ) -> Result<(), EngineError> {
        apply_patch(source, patch, dest, header, limits)
    }
}

/// Every patch format the client ships. `stage_full` downloads the full object
/// for anything not listed here.
static APPLIERS: &[&dyn PatchApplier] = &[&QbsdiffApplier];

fn applier_for(strategy: &Strategy) -> Option<&'static dyn PatchApplier> {
    APPLIERS
        .iter()
        .copied()
        .find(|applier| &applier.strategy() == strategy)
}

pub(crate) struct PrepareInput<'a> {
    pub manifest: &'a Manifest,
    pub diff: &'a DiffResult,
    pub fetcher: &'a dyn Fetcher,
    pub install_root: &'a Path,
    pub staging_root: &'a Path,
    /// Journal root; per-file patch fallbacks are appended to `worker.log` here.
    pub journal_root: &'a Path,
    pub version: &'a str,
    pub manifest_hash: &'a str,
    pub limits: &'a Limits,
    pub cancel: &'a CancellationToken,
    pub progress: Arc<dyn Fn(Progress) + Send + Sync>,
}

struct StageContext<'a> {
    fetcher: &'a dyn Fetcher,
    files_root: &'a Path,
    patches_root: &'a Path,
    journal_root: &'a Path,
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

    preflight(
        input.manifest,
        input.diff,
        input.install_root,
        input.staging_root,
        input.limits,
    )?;
    let repairs = verify_unchanged(input).await?;

    let downloaded = Mutex::new(0);
    let repair_bytes = repairs.iter().fold(0_u64, |total, path| {
        total.saturating_add(
            input
                .manifest
                .files
                .get(path)
                .map(|entry| entry.size)
                .unwrap_or(0),
        )
    });
    let context = StageContext {
        fetcher: input.fetcher,
        files_root: &files_root,
        patches_root: &patches_root,
        journal_root: input.journal_root,
        limits: input.limits,
        cancel: input.cancel,
        progress: input.progress.as_ref(),
        total_download: input.diff.download_bytes.saturating_add(repair_bytes),
        downloaded: &downloaded,
    };

    // Own the work items: async blocks over `&ModifiedFile`/`&String` make the
    // returned future non-general over lifetimes, which breaks Tauri's command
    // macros downstream.
    let limit = input.limits.max_concurrent_downloads.max(1);
    let mut modified = input.diff.modified.clone();
    for path in repairs {
        let mut entry = input.manifest.files.get(&path).cloned().ok_or_else(|| {
            EngineError::ManifestInvalid(format!("{path}: repaired entry is missing"))
        })?;
        if entry.full_key.is_none() {
            return Err(PatchError::SourceMismatch(format!(
                "{path}: local file changed after check and the manifest has no full fallback"
            ))
            .into());
        }
        // A missing or modified local file cannot be a patch source. Repair it
        // from the full blob instead of trying (and failing) the delta path.
        entry.strategy = Strategy::Full;
        entry.delta_key = None;
        entry.delta_size = None;
        tracing::warn!(
            path,
            "local file changed after check; repairing from the full blob"
        );
        modified.push(ModifiedFile { path, entry });
    }
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
        schema: crate::staging::plan::PREPARED_SCHEMA,
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
    install_root: &Path,
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
    // Staging may live on another volume (`staging_dir` is configurable). The
    // install volume then needs room for the materialized files; backups and
    // trash are renames inside the same tree, so they add no bytes. On the same
    // volume this check is already implied by the staging check above.
    let install_available =
        fs4::available_space(install_root).map_err(|err| io_error(err, "install"))?;
    if install_available < materialized {
        return Err(EngineError::InsufficientDiskSpace {
            required: materialized,
            available: install_available,
        });
    }
    Ok(())
}

/// Re-hash every unchanged file in place. Files that no longer match are
/// returned as repair candidates instead of failing the whole prepare; the
/// caller can materialize them from their content-addressed full blob. The
/// install tree can be large, so the scan runs on the blocking pool.
async fn verify_unchanged(input: &PrepareInput<'_>) -> Result<Vec<String>, EngineError> {
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
    let progress = Arc::clone(&input.progress);
    tokio::task::spawn_blocking(move || {
        verify_unchanged_files(&unchanged, &install_root, &cancel, progress.as_ref())
    })
    .await
    .map_err(|err| EngineError::Internal(format!("verify task: {err}")))?
}

fn verify_unchanged_files(
    unchanged: &[(String, String)],
    install_root: &Path,
    cancel: &CancellationToken,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Vec<String>, EngineError> {
    use std::sync::atomic::{AtomicU64, Ordering};

    use rayon::prelude::*;

    let total = unchanged.len() as u64;
    let done = AtomicU64::new(0);
    // Hashing is per-file and independent, so it scales linearly with cores.
    // The pool is bounded because each worker keeps one file open and a large
    // server would otherwise try to read the whole install tree at once.
    let threads = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .min(8);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|err| EngineError::Internal(format!("hash pool: {err}")))?;

    pool.install(|| {
        unchanged
            .par_iter()
            .map(|(path, expected)| -> Result<Option<String>, EngineError> {
                check_cancel(cancel)?;
                let repair = match sha256_file(&install_root.join(path)) {
                    Ok(actual) if actual.eq_ignore_ascii_case(expected) => None,
                    Ok(_) => Some(path.clone()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(path.clone()),
                    Err(err) => return Err(io_error(err, path)),
                };
                // Keep the event rate fixed at one per 50 files regardless of
                // how many workers finish at once.
                let completed = done.fetch_add(1, Ordering::Relaxed) + 1;
                if completed.is_multiple_of(50) || completed == total {
                    progress(Progress::Scanning {
                        file: path.clone(),
                        completed,
                        total,
                    });
                }
                Ok(repair)
            })
            .collect::<Result<Vec<_>, EngineError>>()
            .map(|outcomes| outcomes.into_iter().flatten().collect())
    })
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

    // A cancelled or interrupted prepare may already have produced the final
    // staged file. Reuse it after a hash check instead of downloading/patching
    // the same bytes again.
    if dest.exists() {
        if verify_staged(path, &dest, entry).await.is_ok() {
            let planned = if should_try_patch(entry, context.limits) {
                entry.delta_size.unwrap_or(entry.size)
            } else {
                entry.size
            };
            report_download(context, path, planned);
            (context.progress)(Progress::Verifying { file: path.clone() });
            return Ok(StagedFile {
                path: path.clone(),
                staging_path: dest,
                sha256: entry.sha256.clone(),
                size: entry.size,
            });
        }
        let _ = std::fs::remove_file(&dest);
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
                // Durable evidence for support bundles: the log is the only
                // place that records which file fell back and why.
                crate::worker_log::append(
                    context.journal_root,
                    &format!("patch fallback: {path}: {}", error.code()),
                );
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
    // A cancelled prepare keeps the downloaded patch. Reuse it when the size
    // matches; the target hash check after applying still rejects bad bytes.
    let patch_ready = std::fs::metadata(&patch_path).is_ok_and(|meta| meta.len() == delta_size);
    if !patch_ready {
        download_verified(
            context.fetcher,
            delta_key,
            &patch_path,
            delta_size,
            None,
            context.cancel,
        )
        .await?;
    }
    report_download(context, path, delta_size);

    // Read only the fixed-size header; the payload is parsed by `apply_patch`.
    // Loading the whole patch here would double peak memory for large files.
    let mut header_bytes = [0_u8; PATCH_HEADER_LEN];
    let mut patch_file = std::fs::File::open(&patch_path).map_err(|err| io_error(err, path))?;
    patch_file
        .read_exact(&mut header_bytes)
        .map_err(|_| PatchError::FormatMismatch(format!("{path}: patch is truncated")))?;
    drop(patch_file);
    let applier = applier_for(&entry.strategy)
        .ok_or_else(|| PatchError::UnknownStrategy(entry.strategy.as_str().to_string()))?;
    let header = applier.parse_header(&header_bytes, entry, path)?;

    let source_owned = source.to_path_buf();
    let patch_owned = patch_path.clone();
    let dest_owned = dest.to_path_buf();
    let limits_owned = context.limits.clone();
    tokio::task::spawn_blocking(move || {
        applier.apply(
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

fn validate_header(entry: &FileEntry, header: &PatchHeader, path: &str) -> Result<(), EngineError> {
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
    match &entry.strategy {
        Strategy::Full | Strategy::QbsdiffZstd => {}
        Strategy::Unknown(value) => tracing::warn!(
            path,
            strategy = %value,
            "unknown patch strategy; downloading the full object"
        ),
        strategy => tracing::warn!(
            path,
            strategy = strategy.as_str(),
            "manifest uses a patch strategy this client does not implement; downloading the full object"
        ),
    }
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
        // A same-size but corrupt object must not be resumed from EOF: most
        // object stores answer `Range: bytes=<size>-` with 416, which is not
        // retryable. Drop it and start a clean download.
        let _ = std::fs::remove_file(dest);
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
        if max_bytes > 0 && resume >= max_bytes {
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
    applier_for(&entry.strategy).is_some()
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
