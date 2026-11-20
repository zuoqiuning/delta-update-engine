use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use semver::Version;
use tokio_util::sync::CancellationToken;

use crate::config::EngineConfig;
use crate::crypto::{sha256_bytes, verify_manifest};
use crate::error::{EngineError, SignatureError};
use crate::history;
use crate::journal::state::{self, CommitStatus, PREPARED_JSON, UPDATE_LOCK};
use crate::journal::UpdateLock;
use crate::manifest::diff::DiffResult;
use crate::manifest::model::Manifest;
use crate::manifest::parse_manifest;
use crate::recovery::{self, RecoveryRoots};
use crate::staging::{
    prepare as stage, verify_plan, verify_plan_with, PrepareInput, PreparedPlan, VerifiedInventory,
};
use crate::version::{decide, VersionDecision};

mod commit;
mod scan;

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CheckOutcome {
    pub available: bool,
    pub version: Option<String>,
    pub current_version: String,
    /// Changed files including removals, for display.
    pub files_changed: u32,
    /// Files that will be materialized in staging (modified + added).
    pub files_staged: u32,
    pub download_bytes: u64,
    pub total_bytes: u64,
    /// Release notes from the manifest, if any.
    pub notes: Option<String>,
    /// The publisher marked this update as mandatory.
    pub mandatory: bool,
    /// The running version is below the manifest's `min_supported_version`.
    pub update_required: bool,
    /// The publisher's minimum supported version, when the manifest sets one.
    pub min_supported_version: Option<String>,
    /// Files skipped because they are cloud placeholders (OneDrive/Files
    /// On-Demand). They are not part of the diff, so if the manifest lists them
    /// they are downloaded in full. Surfaced so UIs can explain the size.
    pub cloud_placeholders_skipped: u32,
    /// The manifest was suppressed because the user skipped this exact version.
    /// UIs can offer "run it anyway" instead of claiming the app is current.
    pub skipped_version: Option<String>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Progress {
    Downloading {
        file: String,
        total: u64,
        completed: u64,
    },
    Applying {
        file: String,
    },
    Verifying {
        file: String,
    },
    /// In-place verification of unchanged files before downloads start.
    Scanning {
        file: String,
        completed: u64,
        total: u64,
    },
    Done,
}

#[derive(Debug, Clone)]
pub struct PreparedUpdate {
    pub version: String,
    pub staging_root: PathBuf,
    pub manifest_hash: String,
}

#[derive(Debug, Clone)]
pub struct PendingCommit {
    pub commit_id: String,
    pub version: String,
    pub journal_path: PathBuf,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum StartupOutcome {
    Clean,
    Recovered {
        ops_completed: usize,
        version: String,
    },
    Committed {
        version: String,
    },
    RolledBack {
        to_version: String,
        reason: String,
    },
    ReusedPrepared {
        version: String,
    },
    NeedsAttention {
        error: EngineError,
    },
}

/// Acquire the update lock with a short backoff.
///
/// `confirm_healthy` can race a prepare that holds the lock for the whole
/// download. Failing immediately would surface as "confirmation failed" even
/// though the right answer is "wait a moment"; three attempts cover the common
/// case without blocking shutdown paths.
///
/// This is synchronous and sleeps up to 600ms total. Callers on an async
/// runtime accept that block; if the retry budget grows, wrap this in
/// `spawn_blocking` instead of extending the sleep.
fn acquire_update_lock_retrying(journal_root: &Path) -> Result<UpdateLock, EngineError> {
    let path = journal_root.join(UPDATE_LOCK);
    let mut delay = std::time::Duration::from_millis(200);
    for attempt in 0..3 {
        match UpdateLock::acquire(&path) {
            Ok(lock) => return Ok(lock),
            Err(EngineError::UpdateLocked) => {
                if attempt < 2 {
                    std::thread::sleep(delay);
                    delay *= 2;
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(EngineError::UpdateLocked)
}

/// Envelope schema version, independent of the manifest schema.
const ENVELOPE_SCHEMA: u32 = 2;

#[derive(serde::Deserialize)]
struct EnvelopePayload {
    schema: u32,
    /// The exact manifest JSON text that was signed.
    manifest: String,
    signature: String,
}

/// Verify a single-object envelope and return the exact manifest bytes.
///
/// The manifest is embedded as text rather than as a JSON object so the bytes
/// the signature covers are unambiguous; re-serializing an object could change
/// key order and break verification.
fn verify_envelope(
    raw: &[u8],
    keys: &[String],
    max_bytes: u64,
) -> Result<(Vec<u8>, String), EngineError> {
    if raw.len() as u64 > max_bytes {
        return Err(EngineError::ManifestTooLarge(format!(
            "envelope is {} bytes, limit is {max_bytes}",
            raw.len()
        )));
    }
    let envelope: EnvelopePayload = serde_json::from_slice(raw)
        .map_err(|err| EngineError::ManifestInvalid(format!("envelope: {err}")))?;
    if envelope.schema != ENVELOPE_SCHEMA {
        return Err(EngineError::ManifestInvalid(format!(
            "envelope schema {} is not supported",
            envelope.schema
        )));
    }
    verify_manifest(envelope.manifest.as_bytes(), &envelope.signature, keys)?;
    Ok((envelope.manifest.into_bytes(), envelope.signature))
}

/// Attachment ids declared by a manifest that has not verified yet.
///
/// The field is only a hint for which sibling signatures to try, and every id
/// is shape-checked before it is used in a URL.
fn declared_attachment_ids(raw: &[u8]) -> Vec<String> {
    serde_json::from_slice::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| value.get("signing_key_ids").cloned())
        .and_then(|ids| ids.as_array().cloned())
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str())
                .filter(|id| crate::crypto::is_key_id(id))
                .take(crate::manifest::parse::MAX_SIGNING_KEY_IDS)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Attach the publish-window hint to a signature failure that survived the
/// retries. The code and `retryable` stay untouched on purpose: a tampered
/// manifest produces the same mismatch, and downgrading it to "try later"
/// would hide an attack behind an operational message.
fn publish_window_hint(error: EngineError, attempts: usize) -> EngineError {
    match error {
        EngineError::Signature(SignatureError::Invalid(message)) => {
            EngineError::Signature(SignatureError::Invalid(format!(
                "{message} (manifest and signature disagreed after {attempts} attempts; a release may be in progress)"
            )))
        }
        other => other,
    }
}

/// The manifest that passed signature verification in the last successful
/// check, plus the diff computed against it. Prepare consumes this snapshot so
/// callers can never substitute an unverified manifest.
#[derive(Clone)]
struct VerifiedUpdate {
    manifest: Manifest,
    manifest_hash: String,
    raw_manifest: Vec<u8>,
    signature: String,
    diff: DiffResult,
}

struct SessionState {
    verified: Option<VerifiedUpdate>,
    cancel: CancellationToken,
    /// Staged files hashed by the prepare that produced `prepared.json`.
    inventory: Option<Arc<VerifiedInventory>>,
    /// Old-file hashes from the last check, for the files the diff will replace
    /// or remove. `begin_commit` reuses them while size and mtime still match
    /// instead of reading the old bytes a second time.
    scan_inventory: Option<Arc<VerifiedInventory>>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            verified: None,
            cancel: CancellationToken::new(),
            inventory: None,
            scan_inventory: None,
        }
    }
}

/// Main engine handle. Holds configuration and no Tauri types.
pub struct Engine {
    config: EngineConfig,
    current_version: Version,
    session: Mutex<SessionState>,
    /// Test/legacy escape hatch. Not part of `EngineConfig`: production code
    /// cannot reach it accidentally, only through [`Engine::new_legacy`].
    allow_unsigned_journal_recovery: bool,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        Self::build(config, false)
    }

    /// Test/legacy constructor that permits replaying a journal without a
    /// signed-manifest binding. `#[doc(hidden)]`: it is not a supported
    /// production option, and the plugin/CLI never call it.
    #[doc(hidden)]
    pub fn new_legacy(
        config: EngineConfig,
        allow_unsigned_journal_recovery: bool,
    ) -> Result<Self, EngineError> {
        Self::build(config, allow_unsigned_journal_recovery)
    }

    fn build(config: EngineConfig, allow_unsigned: bool) -> Result<Self, EngineError> {
        config.validate()?;
        let current_version = Version::parse(&config.current_version)
            .map_err(|_| EngineError::VersionInvalid(config.current_version.clone()))?;
        if allow_unsigned {
            tracing::warn!(
                "allow_unsigned_journal_recovery is enabled: recovery may execute a journal that is not bound to a signed manifest"
            );
        }
        Ok(Self {
            config,
            current_version,
            session: Mutex::new(SessionState::default()),
            allow_unsigned_journal_recovery: allow_unsigned,
        })
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Fetch and verify the manifest, retrying the pair a couple of times.
    ///
    /// `latest.json` and `latest.json.minisig` are two separate objects, so a
    /// release briefly exposes a manifest with the previous or next signature.
    /// Retrying is safe: nothing is parsed or cached until the pair verifies.
    /// Exhausting the retries returns the real signature error with a hint that
    /// a release may be in flight; inventing a "try later" code would make real
    /// tampering indistinguishable from a publish window.
    async fn fetch_verified_manifest(&self) -> Result<(Vec<u8>, String), EngineError> {
        // The envelope carries the manifest and its signature in one object, so
        // a reader can never land between the two writes. An envelope that is
        // present but does not verify is fatal: falling back to the legacy pair
        // would let an attacker push clients onto the windowed path.
        match self.config.fetcher.fetch_envelope().await {
            Ok(Some(envelope)) => {
                return verify_envelope(
                    &envelope,
                    &self.config.signing_public_keys,
                    self.config.limits.manifest_max_bytes,
                )
            }
            Ok(None) => {}
            Err(error) => tracing::debug!(
                %error,
                "envelope unavailable; falling back to latest.json + .minisig"
            ),
        }
        const BACKOFF_MS: [u64; 2] = [200, 600];
        let attempts = BACKOFF_MS.len() + 1;
        let mut last = None;
        for attempt in 0..attempts {
            let (raw_manifest, raw_signature) = tokio::try_join!(
                self.config.fetcher.fetch_manifest(),
                self.config.fetcher.fetch_signature()
            )?;
            let signature = std::str::from_utf8(&raw_signature)
                .map_err(|_| {
                    EngineError::Signature(SignatureError::Invalid(
                        "signature is not valid UTF-8".into(),
                    ))
                })?
                .to_string();
            match verify_manifest(&raw_manifest, &signature, &self.config.signing_public_keys) {
                Ok(()) => return Ok((raw_manifest, signature)),
                Err(error) => {
                    if let Some(accepted) = self.try_attachments(&raw_manifest).await {
                        return Ok((raw_manifest, accepted));
                    }
                    last = Some(error);
                    if let Some(delay) = BACKOFF_MS.get(attempt) {
                        tokio::time::sleep(std::time::Duration::from_millis(*delay)).await;
                    }
                }
            }
        }
        Err(publish_window_hint(
            last.unwrap_or(EngineError::Signature(SignatureError::NoTrustedKey)),
            attempts,
        ))
    }

    /// Try the additional signatures the manifest declares. Ids are read from
    /// unverified bytes, so only 16-char hex ids are accepted; a tampered
    /// manifest can at most make us fetch sibling `.minisig.<id>` objects.
    async fn try_attachments(&self, raw_manifest: &[u8]) -> Option<String> {
        for key_id in declared_attachment_ids(raw_manifest) {
            let Ok(Some(bytes)) = self.config.fetcher.fetch_attachment(&key_id).await else {
                continue;
            };
            let Ok(signature) = std::str::from_utf8(&bytes) else {
                continue;
            };
            if verify_manifest(raw_manifest, signature, &self.config.signing_public_keys).is_ok() {
                tracing::info!(
                    key_id = %key_id,
                    "accepted an additional manifest signature"
                );
                return Some(signature.to_string());
            }
        }
        None
    }

    /// Fetch, verify, parse, apply channel/version guards, then diff against
    /// the install root. The verified snapshot is cached for `prepare`.
    pub async fn check(&self) -> Result<CheckOutcome, EngineError> {
        let (raw_manifest, signature) = self.fetch_verified_manifest().await?;
        let manifest = parse_manifest(&raw_manifest, &self.config.limits)?;
        let channel = manifest.channel.as_deref().unwrap_or("stable");
        if channel != self.config.channel {
            self.clear_verified()?;
            return Ok(self.outcome(false, None, 0, 0, 0, 0));
        }
        // Only a manifest for this client's channel can require an update.
        let min_supported_version = manifest.min_supported_version.clone();
        let update_required = manifest.is_update_required(&self.current_version);
        let with_support = |mut outcome: CheckOutcome| {
            outcome.update_required = update_required;
            outcome.min_supported_version = min_supported_version.clone();
            outcome
        };

        if state::read_skipped_version(&self.config.journal_root).as_deref()
            == Some(manifest.version.as_str())
        {
            if !manifest.rollback {
                self.clear_verified()?;
                let mut outcome = self.outcome(false, None, 0, 0, 0, 0);
                outcome.skipped_version = Some(manifest.version.clone());
                return Ok(with_support(outcome));
            }
            // An explicit rollback to this version clears the crash-loop skip.
            let _ = std::fs::remove_file(self.config.journal_root.join(state::SKIPPED_VERSION));
        }

        let current_version = self.current_version.to_string();
        let decision = decide(&manifest.version, &current_version, manifest.rollback)?;
        if decision == VersionDecision::NoUpdate {
            self.clear_verified()?;
            return Ok(with_support(self.outcome(false, None, 0, 0, 0, 0)));
        }

        let (diff, placeholders_skipped, scan_inventory) = tokio::task::spawn_blocking({
            let install_root = self.config.install_root.clone();
            let journal_root = self.config.journal_root.clone();
            let manifest = manifest.clone();
            move || scan::diff_install(&install_root, &journal_root, &manifest)
        })
        .await
        .map_err(|err| EngineError::Internal(format!("scan task: {err}")))??;
        let files_staged =
            u32::try_from(diff.modified.len() + diff.added.len()).unwrap_or(u32::MAX);
        let files_changed =
            files_staged.saturating_add(u32::try_from(diff.removed.len()).unwrap_or(u32::MAX));
        let mut outcome = self.outcome(
            true,
            Some(manifest.version.clone()),
            files_changed,
            files_staged,
            diff.download_bytes,
            diff.total_bytes,
        );
        outcome.notes = manifest.notes.clone();
        outcome.mandatory = manifest.mandatory;
        outcome.cloud_placeholders_skipped = placeholders_skipped;
        self.set_verified(
            VerifiedUpdate {
                manifest,
                manifest_hash: sha256_bytes(&raw_manifest),
                raw_manifest,
                signature,
                diff,
            },
            scan_inventory,
        )?;
        Ok(with_support(outcome))
    }

    /// Scan the install root and classify every manifest entry against it.
    ///
    /// `.old/` and `.trash/` are bookkeeping directories owned by the update
    /// process and never participate in a user-visible diff. File hashes are
    /// cached by size + mtime under the journal root so repeated checks of a
    /// large install do not re-hash unchanged files. `check` runs this on the
    /// blocking pool.
    pub fn diff(&self, local_root: &Path, remote: &Manifest) -> Result<DiffResult, EngineError> {
        scan::diff_install(local_root, &self.config.journal_root, remote).map(|(diff, _, _)| diff)
    }

    /// Prepare the version that passed `check`. Runs a check first when no
    /// verified manifest is cached, mirroring the documented command contract.
    pub async fn prepare(
        &self,
        progress: Box<dyn Fn(Progress) + Send + Sync>,
    ) -> Result<PreparedUpdate, EngineError> {
        let cancel = {
            let mut session = self
                .session
                .lock()
                .map_err(|_| EngineError::Internal("engine state poisoned".into()))?;
            session.cancel = CancellationToken::new();
            session.cancel.clone()
        };
        let verified = self.ensure_verified().await?;
        // Hold the cross-process lock across staging and PREPARED persistence so
        // two instances can never interleave writes to the same journal root.
        let _lock = UpdateLock::acquire(&self.config.journal_root.join(UPDATE_LOCK))?;
        let progress: Arc<dyn Fn(Progress) + Send + Sync> = Arc::from(progress);
        let (plan, inventory) = stage(&PrepareInput {
            manifest: &verified.manifest,
            diff: &verified.diff,
            fetcher: self.config.fetcher.as_ref(),
            install_root: &self.config.install_root,
            staging_root: &self.config.staging_root,
            journal_root: &self.config.journal_root,
            version: &verified.manifest.version,
            manifest_hash: &verified.manifest_hash,
            limits: &self.config.limits,
            cancel: &cancel,
            progress: Arc::clone(&progress),
        })
        .await?;
        let inventory = Arc::new(inventory);
        // `begin_commit` re-verifies the persisted plan; remember which staged
        // files this process already hashed so it does not read them again.
        self.session
            .lock()
            .map_err(|_| EngineError::Internal("engine state poisoned".into()))?
            .inventory = Some(Arc::clone(&inventory));
        // Hashing every staged file can take a while; keep it off the async
        // runtime so downloads and command handling stay responsive.
        let plan = tokio::task::spawn_blocking(move || {
            verify_plan_with(&plan, &inventory)?;
            Ok::<PreparedPlan, EngineError>(plan)
        })
        .await
        .map_err(|err| EngineError::Internal(format!("verify task: {err}")))??;
        plan.save(&self.config.journal_root.join("prepared.json"))?;
        progress(Progress::Done);
        Ok(PreparedUpdate {
            version: plan.version,
            staging_root: plan.staging_root,
            manifest_hash: plan.manifest_hash,
        })
    }

    /// Cancel the prepare currently in flight. Temporary files are kept for
    /// resume by the next prepare attempt.
    pub fn cancel_prepare(&self) -> Result<(), EngineError> {
        self.session
            .lock()
            .map_err(|_| EngineError::Internal("engine state poisoned".into()))?
            .cancel
            .cancel();
        Ok(())
    }

    /// Startup entry point: replay a pending journal, detect a crash loop and
    /// roll back, reuse a PREPARED plan, or clean up.
    ///
    /// Counts one startup attempt. Use [`Self::recover_after_elevation`] when
    /// an elevated worker already performed the recovery for this start.
    pub fn recover_and_cleanup(&self) -> Result<StartupOutcome, EngineError> {
        let attempts =
            state::increment_attempts(&self.config.journal_root, self.config.crash_loop_threshold)?;
        let outcome = self.recover_with_attempts(attempts)?;
        self.record_startup_outcome(&outcome);
        Ok(outcome)
    }

    /// Read the newest persisted update outcomes for the host application to
    /// display or forward to its own monitoring. Nothing is uploaded here.
    pub fn update_history(&self, limit: usize) -> Vec<crate::history::UpdateRecord> {
        history::read(&self.config.journal_root, limit)
    }

    fn record_startup_outcome(&self, outcome: &StartupOutcome) {
        match outcome {
            StartupOutcome::RolledBack { reason, .. } => {
                history::record_rolled_back(&self.config.journal_root, reason);
            }
            StartupOutcome::NeedsAttention { error } => {
                history::record_failed(&self.config.journal_root, error.code());
            }
            _ => {}
        }
    }

    /// A normal shutdown of a committed but unconfirmed version proves the
    /// process did not crash. Discount the startup attempt so "open, look,
    /// close" does not accumulate into a rollback; real crashes never reach the
    /// exit hook and keep counting.
    ///
    /// Callers gate this on a grace period and on the update awaiting
    /// confirmation. They must also ensure the commit really reached
    /// `CommitStatus::Completed`: this method deliberately no-ops otherwise, so
    /// a future refactor that splits `begin_commit` from the worker spawn must
    /// not call it between those steps.
    pub fn discount_clean_exit(&self) -> Result<(), EngineError> {
        let journal_root = &self.config.journal_root;
        if !matches!(
            state::read_commit_status(journal_root)?,
            Some(CommitStatus::Completed { .. })
        ) {
            return Ok(());
        }
        let _lock = UpdateLock::acquire(&journal_root.join(UPDATE_LOCK))?;
        let attempts = state::read_attempts(journal_root);
        if attempts == 0 {
            return Ok(());
        }
        state::write_attempts(journal_root, attempts - 1, self.config.crash_loop_threshold)?;
        tracing::info!(
            remaining = attempts - 1,
            "clean exit discounted one unconfirmed start"
        );
        Ok(())
    }

    /// Inspect the result of an elevated recovery worker without counting a
    /// second startup attempt. The worker increments the counter once.
    pub fn recover_after_elevation(&self) -> Result<StartupOutcome, EngineError> {
        let outcome =
            self.recover_with_attempts(state::read_attempts(&self.config.journal_root))?;
        self.record_startup_outcome(&outcome);
        Ok(outcome)
    }

    /// Whether recovering this startup needs write access to the install root.
    ///
    /// Only a pending journal, a failed commit, or a completed commit past the
    /// crash-loop threshold needs writes; a normal start after an update does
    /// not.
    pub fn recovery_needs_install_write(&self) -> Result<bool, EngineError> {
        recovery::needs_install_write(&self.config.journal_root, self.config.crash_loop_threshold)
    }

    fn recover_with_attempts(&self, attempts: u64) -> Result<StartupOutcome, EngineError> {
        recovery::recover(&RecoveryRoots {
            install_root: &self.config.install_root,
            staging_root: &self.config.staging_root,
            journal_root: &self.config.journal_root,
            crash_loop_threshold: self.config.crash_loop_threshold,
            attempts,
            trusted_keys: &self.config.signing_public_keys,
            accept_unsigned_journal: self.allow_unsigned_journal_recovery,
        })
    }

    /// Rebuild the persisted PREPARED update after a restart.
    pub fn resume_prepared(&self) -> Result<PreparedUpdate, EngineError> {
        let path = self.config.journal_root.join(PREPARED_JSON);
        if !path.exists() {
            return Err(EngineError::NotPrepared);
        }
        let plan = PreparedPlan::load(&path)?;
        verify_plan(&plan)?;
        Ok(PreparedUpdate {
            version: plan.version,
            staging_root: plan.staging_root,
            manifest_hash: plan.manifest_hash,
        })
    }

    async fn ensure_verified(&self) -> Result<VerifiedUpdate, EngineError> {
        if let Some(verified) = self.cached_verified()? {
            return Ok(verified);
        }
        self.check().await?;
        self.cached_verified()?.ok_or(EngineError::NotPrepared)
    }

    fn cached_verified(&self) -> Result<Option<VerifiedUpdate>, EngineError> {
        Ok(self
            .session
            .lock()
            .map_err(|_| EngineError::Internal("engine state poisoned".into()))?
            .verified
            .clone())
    }

    fn staged_inventory(&self) -> Option<Arc<VerifiedInventory>> {
        self.session
            .lock()
            .ok()
            .and_then(|session| session.inventory.clone())
    }

    fn set_verified(
        &self,
        verified: VerifiedUpdate,
        scan_inventory: VerifiedInventory,
    ) -> Result<(), EngineError> {
        state::write_verified_manifest(
            &self.config.journal_root,
            &verified.raw_manifest,
            &verified.signature,
        )?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| EngineError::Internal("engine state poisoned".into()))?;
        session.verified = Some(verified);
        session.scan_inventory = Some(Arc::new(scan_inventory));
        Ok(())
    }

    fn clear_verified(&self) -> Result<(), EngineError> {
        let mut session = self
            .session
            .lock()
            .map_err(|_| EngineError::Internal("engine state poisoned".into()))?;
        session.verified = None;
        session.scan_inventory = None;
        Ok(())
    }

    fn scan_inventory(&self) -> Option<Arc<VerifiedInventory>> {
        self.session
            .lock()
            .ok()
            .and_then(|session| session.scan_inventory.clone())
    }

    fn outcome(
        &self,
        available: bool,
        version: Option<String>,
        files_changed: u32,
        files_staged: u32,
        download_bytes: u64,
        total_bytes: u64,
    ) -> CheckOutcome {
        CheckOutcome {
            available,
            version,
            current_version: self.config.current_version.clone(),
            files_changed,
            files_staged,
            download_bytes,
            total_bytes,
            notes: None,
            mandatory: false,
            update_required: false,
            min_supported_version: None,
            cloud_placeholders_skipped: 0,
            skipped_version: None,
        }
    }
}
