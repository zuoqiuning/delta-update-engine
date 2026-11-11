use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

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
    Release(ReleaseArgs),
    /// Verify a published version from the object store.
    Verify(VerifyArgs),
    /// Check that a secret key matches a public key.
    VerifyKey(VerifyKeyArgs),
    /// Inspect a published endpoint and its artifacts.
    Doctor(DoctorArgs),
    /// Inspect a local journal directory (offline update diagnostics).
    Inspect(InspectArgs),
}

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
    /// COS region (defaults to COS_REGION).
    #[arg(long)]
    pub region: Option<String>,
    /// Prefix for every object and manifest key.
    #[arg(long)]
    pub prefix: Option<String>,
    /// Generate patches and the signed manifest without uploading.
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
    /// qbsdiff needs roughly 17x the input size in memory; raise this only if
    /// the release machine has enough RAM and `--jobs` is kept low.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    pub patch_max_file_size: u64,
    /// Run verify after publishing.
    #[arg(long)]
    pub verify: bool,
    /// Patch generation parallelism (defaults to available cores).
    #[arg(long)]
    pub jobs: Option<usize>,
    /// Print the per-file strategy table.
    #[arg(long)]
    pub verbose: bool,
}

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
    /// Number of entries to download and verify.
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
pub struct DoctorArgs {
    /// Manifest endpoint; repeat for multiple mirrors.
    #[arg(long, required = true)]
    pub endpoint: Vec<String>,
    /// Trusted public key file; omitted means structural checks only.
    #[arg(long)]
    pub public_key: Option<PathBuf>,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Path to the app's journal directory (contains commit.journal/apply.json).
    #[arg(long)]
    pub journal_dir: PathBuf,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}
