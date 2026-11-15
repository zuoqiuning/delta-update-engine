use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "delta-update",
    version,
    about = "Build and publish delta updates"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Generate patches, manifest and signature, then publish.
    #[command(after_help = "\
典型场景:
  首次发布   release --to ./dist --version 1.0.0 --bucket ./channel --key <release.key> --initial
  常规增量   release --from ./dist-1.0.0 --from-version 1.0.0 --to ./dist-1.1.0 --version 1.1.0 --bucket ./channel --key <release.key>
   CI 一体化  release ... --pipeline --report release-report.json
  紧急回滚   rollback --bucket ./channel --to-version 1.0.0 --key <release.key>
路径、版本与密钥请按项目替换。")]
    Release(ReleaseArgs),
    /// Verify a published version from the object store.
    Verify(VerifyArgs),
    /// Check that a secret key matches a public key.
    VerifyKey(VerifyKeyArgs),
    /// Print the public key and key ID derived from a secret key.
    KeyInfo(KeyInfoArgs),
    /// Inspect a published endpoint and its artifacts.
    Doctor(DoctorArgs),
    /// Inspect a local journal directory (offline update diagnostics).
    Inspect(InspectArgs),
    /// Print the Tauri plugin configuration and capability snippets.
    Init(InitArgs),
    /// Print a migration plan from the official tauri-plugin-updater.
    Migrate(MigrateArgs),
    /// Summarize release reports written by `release --pipeline`.
    Report(ReportArgs),
    /// Serve a release directory over HTTP for local demos and debugging.
    #[command(
        after_help = "仅用于本地开发与演示：默认只绑定 127.0.0.1。loopback 不是信任边界，不防御同机恶意进程；不要用 socat / netsh portproxy 转发到公网。该服务没有 TLS、鉴权或缓存。"
    )]
    Serve(ServeArgs),
    /// Point `latest.json` at an already published version (incident response).
    Rollback(RollbackArgs),
}

/// Generate patches, manifest and signature, then publish.
///
/// `cos://` reads COS_SECRET_ID / COS_SECRET_KEY (optional COS_SESSION_TOKEN,
/// COS_REGION). `s3://` reads S3_ACCESS_KEY_ID / S3_SECRET_ACCESS_KEY (optional
/// S3_SESSION_TOKEN, S3_REGION, S3_ENDPOINT), with AWS_* aliases. A local
/// directory needs no credentials.
#[derive(Debug, Args)]
pub struct ReleaseArgs {
    /// New build output directory.
    #[arg(long)]
    pub to: PathBuf,
    /// New version (SemVer), written into the manifest.
    #[arg(long)]
    pub version: String,
    /// Object store: cos://bucket, s3://bucket, or a local directory.
    #[arg(long)]
    pub bucket: String,
    /// minisign secret key file (path only; content never goes on the command line).
    #[arg(long)]
    pub key: PathBuf,
    /// Previous build output directory used as the diff base.
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// Previous version (SemVer); required with --from.
    #[arg(long)]
    pub from_version: Option<String>,
    /// First release: every file is uploaded in full.
    #[arg(long)]
    pub initial: bool,
    /// Publish a lower version with rollback: true.
    #[arg(long)]
    pub rollback: bool,
    /// Update channel written into the manifest.
    #[arg(long, default_value = "stable")]
    pub channel: String,
    /// Release notes file embedded as `notes` in the manifest (UTF-8, max 64 KiB).
    #[arg(long)]
    pub notes_file: Option<PathBuf>,
    /// Mark this update as mandatory for clients.
    #[arg(long)]
    pub mandatory: bool,
    /// Oldest client version still supported. Older clients get
    /// `update_required` from `check`; the application decides what to do.
    #[arg(long)]
    pub min_supported: Option<String>,
    /// COS region (defaults to COS_REGION).
    #[arg(long)]
    pub region: Option<String>,
    /// Prefix for every object and manifest key.
    #[arg(long)]
    pub prefix: Option<String>,
    /// Generate patches and sign the manifest in memory without uploading.
    ///
    /// This is a preflight check; it prints a summary but does not write the
    /// manifest to disk or touch the bucket.
    #[arg(long)]
    pub dry_run: bool,
    /// Fail when a patchable file falls back to a full object.
    #[arg(long)]
    pub require_delta: bool,
    /// Maximum patch/full size ratio before falling back to full.
    #[arg(long, default_value_t = 0.60)]
    pub delta_max_ratio: f64,
    /// Skip diffing files larger than this and publish them in full (default 64 MiB).
    ///
    /// qbsdiff holds the source, target, a 4-byte suffix array per source byte
    /// and the delta in memory, so each parallel job needs several times the
    /// file size. Keep `--jobs` low on large releases, or lower this threshold.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    pub patch_max_file_size: u64,
    /// Run verify after publishing.
    #[arg(long)]
    pub verify: bool,
    /// Simulate apply + rollback on a copy of --from before publishing.
    #[arg(long)]
    pub self_test: bool,
    /// Generate, self-test, upload and audit in one run, then write a report.
    ///
    /// Fails at the first step; the report is only written when every step
    /// succeeded.
    #[arg(long)]
    pub pipeline: bool,
    /// Report path written by --pipeline.
    #[arg(long, default_value = "release-report.json")]
    pub report: PathBuf,
    /// After publishing, keep auditing the bucket for this many seconds.
    #[arg(long, requires = "pipeline")]
    pub watch: Option<u64>,
    /// Seconds between --watch audits.
    #[arg(long, default_value_t = 60)]
    pub interval: u64,
    /// Patch generation parallelism (defaults to available cores).
    #[arg(long)]
    pub jobs: Option<usize>,
    /// Print the per-file strategy table.
    #[arg(long)]
    pub verbose: bool,
}

/// Verify a published version from the object store.
///
/// Cloud buckets use the same credentials as `release`; a local directory needs
/// none.
#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Object store: cos://bucket, s3://bucket, or a local directory.
    #[arg(long)]
    pub bucket: String,
    /// Versioned manifest to verify.
    #[arg(long)]
    pub version: String,
    /// Trusted minisign public key file (one or more keys).
    #[arg(long)]
    pub public_key: PathBuf,
    /// Maximum patch objects and distinct full objects to download and hash-check (0 skips downloads).
    #[arg(long, default_value_t = 3)]
    pub sample: usize,
    /// COS region (defaults to COS_REGION).
    #[arg(long)]
    pub region: Option<String>,
    /// Prefix used by release for object keys.
    #[arg(long)]
    pub prefix: Option<String>,
}

#[derive(Debug, Args)]
pub struct VerifyKeyArgs {
    /// minisign secret key file.
    #[arg(long)]
    pub key: PathBuf,
    /// Comma-separated base64 public keys or a public key file's contents.
    #[arg(long)]
    pub public: String,
}

#[derive(Debug, Args)]
pub struct KeyInfoArgs {
    /// minisign secret key file.
    #[arg(long)]
    pub key: PathBuf,
}

/// Client-visible failure to reproduce with `doctor --simulate-failure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SimulateMode {
    /// Another instance holds `update.lock` (E_UPDATE_LOCKED).
    Lock,
    /// Local files changed under a patch (E_PATCH_SOURCE_MISMATCH).
    Patch,
    /// Staging cannot be materialized (E_DISK_INSUFFICIENT).
    Disk,
}

impl SimulateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Patch => "patch",
            Self::Disk => "disk",
        }
    }
}

/// Patch failure to reproduce with `doctor --simulate-failure patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum PatchReason {
    /// Local files changed after check (E_PATCH_SOURCE_MISMATCH).
    #[default]
    SourceMismatch,
    /// Truncated container (E_PATCH_FORMAT_MISMATCH).
    Format,
    /// Staged bytes do not match the manifest (E_PATCH_TARGET_HASH_MISMATCH).
    TargetHash,
    /// Hashing the patch would exceed the memory budget (E_PATCH_MEMORY_BUDGET).
    Memory,
}

impl PatchReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceMismatch => "source-mismatch",
            Self::Format => "format",
            Self::TargetHash => "target-hash",
            Self::Memory => "memory",
        }
    }
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Manifest endpoint; repeat for multiple mirrors.
    #[arg(long, required_unless_present_any = ["check_config", "simulate_failure"])]
    pub endpoint: Vec<String>,
    /// Trusted public key file; omitted means structural checks only.
    #[arg(long)]
    pub public_key: Option<PathBuf>,
    /// Validate plugins.delta-update in <dir>/src-tauri/tauri.conf.json offline.
    #[arg(long)]
    pub check_config: bool,
    /// Print the payload a real failure produces, without changing anything.
    #[arg(long, value_name = "MODE", conflicts_with_all = ["check_config", "watch"])]
    pub simulate_failure: Option<SimulateMode>,
    /// Patch failure to reproduce; defaults to source-mismatch.
    #[arg(long, value_name = "REASON", requires = "simulate_failure")]
    pub reason: Option<PatchReason>,
    /// Project root for --check-config.
    #[arg(long, default_value = ".")]
    pub dir: PathBuf,
    /// Keep checking for this many seconds; any failed pass fails the command.
    #[arg(long)]
    pub watch: Option<u64>,
    /// Seconds between checks in --watch mode.
    #[arg(long, default_value_t = 60)]
    pub interval: u64,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Path to the app's journal directory (required unless --from-release or --bundle).
    #[arg(long, required_unless_present_any = ["from_release", "bundle"])]
    pub journal_dir: Option<PathBuf>,
    /// Cross-check a diagnostic bundle against the published manifest.
    #[arg(long, conflicts_with_all = ["journal_dir", "export"])]
    pub bundle: Option<PathBuf>,
    /// Audit a published release: cos://bucket, s3://bucket, or a local directory.
    #[arg(
        long,
        requires_all = ["version", "public_key"],
        conflicts_with = "export"
    )]
    pub from_release: Option<String>,
    /// Versioned manifest to audit (with --from-release).
    #[arg(long)]
    pub version: Option<String>,
    /// Trusted minisign public key file (with --from-release).
    #[arg(long)]
    pub public_key: Option<PathBuf>,
    /// Prefix used by release for object keys (with --from-release).
    #[arg(long)]
    pub prefix: Option<String>,
    /// List every journal operation and whether its target is already in place.
    #[arg(long)]
    pub verbose: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
    /// Write a single-file diagnostic bundle (report + worker.log tail + file list).
    #[arg(long)]
    pub export: Option<PathBuf>,
    /// Replace absolute install/journal/user paths with placeholders in the bundle.
    #[arg(long)]
    pub redact: bool,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Project root to inspect for an existing tauri.conf.json.
    #[arg(long, default_value = ".")]
    pub dir: PathBuf,
    /// Manifest endpoint; omitted prints a placeholder.
    #[arg(long)]
    pub endpoint: Option<String>,
    /// Trusted minisign public key; repeat for key rotation. At least one is required.
    #[arg(long = "public-key")]
    pub public_keys: Vec<String>,
    /// Update channel written into the config.
    #[arg(long, default_value = "stable")]
    pub channel: String,
    /// Write src-tauri/capabilities/delta-update.json (never edits tauri.conf.json).
    #[arg(long)]
    pub write: bool,
    /// Capability set: manage (default, no confirm_healthy), monitor (read-only), default (all commands).
    #[arg(
        long,
        default_value = "manage",
        value_parser = ["default", "manage", "monitor"]
    )]
    pub permission_set: String,
}

#[derive(Debug, Args)]
pub struct MigrateArgs {
    /// Project root containing src-tauri/tauri.conf.json.
    #[arg(long, default_value = ".")]
    pub dir: PathBuf,
    /// Write the capability file and delta-update-migration.md; never edits
    /// existing files.
    #[arg(long)]
    pub write: bool,
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// Directory containing `release --pipeline` reports.
    #[arg(long)]
    pub history: PathBuf,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Directory published by `release --bucket <DIR>`.
    #[arg(long)]
    pub dir: PathBuf,
    /// Port to listen on; 0 lets the OS choose.
    #[arg(long, default_value_t = 8123)]
    pub port: u16,
    /// Address to bind. Loopback only unless you explicitly change it.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
}

/// Reuse an already published manifest instead of rebuilding patches.
#[derive(Debug, Args)]
pub struct RollbackArgs {
    /// Object store: cos://bucket, s3://bucket, or a local directory.
    #[arg(long)]
    pub bucket: String,
    /// Version to make the recommended release.
    #[arg(long)]
    pub to_version: String,
    /// minisign secret key used to re-sign the republished manifest.
    #[arg(long)]
    pub key: PathBuf,
    /// COS region (defaults to COS_REGION).
    #[arg(long)]
    pub region: Option<String>,
    /// Prefix used by release for object keys.
    #[arg(long)]
    pub prefix: Option<String>,
    /// Print what would be published without uploading.
    #[arg(long)]
    pub dry_run: bool,
}
