//! `rollback`: point `latest.json` at an already published manifest.

use anyhow::{bail, Context};
use delta_update_engine::limits::Limits;
use delta_update_engine::manifest::parse::parse_manifest;

use crate::cli::RollbackArgs;
use crate::output;
use crate::pipeline::keys::normalize_prefix;
use crate::pipeline::{publish, sign};
use crate::publisher;

pub fn run(args: &RollbackArgs) -> anyhow::Result<()> {
    rollback(args)
}

/// Point `latest.json` at an already published manifest.
///
/// Unlike `release --rollback`, nothing is rebuilt: the versioned manifest and
/// its objects stay byte-identical, only `latest.json` is re-signed with
/// `rollback: true`. This is the incident-response path.
pub fn rollback(args: &RollbackArgs) -> anyhow::Result<()> {
    semver::Version::parse(&args.to_version)
        .with_context(|| format!("invalid --to-version: {}", args.to_version))?;
    let prefix = normalize_prefix(args.prefix.as_deref())?;
    let publisher = publisher::open(&args.bucket, args.region.as_deref())?;
    let public_key = sign::public_key_base64(&args.key)?;
    let (raw, signature) = publish::fetch_manifest(publisher.as_ref(), &prefix, &args.to_version)?;
    delta_update_engine::crypto::verify_manifest(
        &raw,
        &signature,
        std::slice::from_ref(&public_key),
    )
    .context("verify the published manifest with --key")?;
    let parsed = parse_manifest(&raw, &Limits::default())?;
    if parsed.version != args.to_version {
        bail!(
            "manifest under {} reports version {}",
            args.to_version,
            parsed.version
        );
    }

    let mut value: serde_json::Value =
        serde_json::from_slice(&raw).context("parse the published manifest")?;
    value["rollback"] = serde_json::Value::Bool(true);
    value["published_at"] = serde_json::Value::String(
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    let manifest = serde_json::to_vec_pretty(&value).context("serialize rollback manifest")?;
    let signature = sign::sign_manifest(&manifest, &args.key)?;

    if args.dry_run {
        output::info(&format!(
            "dry-run rollback: latest.json -> {} ({} bytes)",
            args.to_version,
            manifest.len()
        ));
        return Ok(());
    }
    let key = publish::publish_latest(publisher.as_ref(), &prefix, &manifest, &signature.bytes)?;
    output::info(&format!(
        "{key} now points at {} (rollback: true); the versioned manifest is unchanged",
        args.to_version
    ));
    Ok(())
}
