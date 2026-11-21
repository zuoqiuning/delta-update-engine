//! `promote`: move an already published version to another channel.
//!
//! Promotion rewrites only the `channel` field and re-signs. Patches and full
//! objects are byte-identical to the canary release, so switching an audited
//! build to `stable` costs one signature instead of a patch regeneration.

use anyhow::{bail, Context};
use delta_update_engine::limits::Limits;
use delta_update_engine::manifest::parse::parse_manifest;

use crate::cli::PromoteArgs;
use crate::output;
use crate::pipeline::keys::normalize_prefix;
use crate::pipeline::{audit, publish, sbom, sign};
use crate::publisher;

pub fn run(args: &PromoteArgs) -> anyhow::Result<()> {
    promote(args)
}

pub fn promote(args: &PromoteArgs) -> anyhow::Result<()> {
    if args.channel.trim().is_empty() || args.channel.chars().any(char::is_whitespace) {
        bail!("--channel must be a non-empty string without whitespace");
    }
    if !args.key.is_file() {
        bail!("--key is not a file: {}", args.key.display());
    }
    let prefix = normalize_prefix(args.prefix.as_deref())?;
    let publisher = publisher::open(&args.bucket, args.region.as_deref())?;
    let public_key = sign::public_key_base64(&args.key)?;
    let (raw, signature) = publish::fetch_manifest(publisher.as_ref(), &prefix, &args.version)
        .with_context(|| {
            format!(
                "read the published manifest for {}; run `release` first",
                args.version
            )
        })?;
    delta_update_engine::crypto::verify_manifest(
        &raw,
        &signature,
        std::slice::from_ref(&public_key),
    )
    .context(
        "verify the published manifest with --key (promotion must re-sign with the original key)",
    )?;
    let parsed = parse_manifest(&raw, &Limits::default())?;
    if parsed.version != args.version {
        bail!(
            "manifest under {} reports version {}",
            args.version,
            parsed.version
        );
    }
    let previous_channel = parsed.channel.clone().unwrap_or_else(|| "stable".into());

    // A rotated release carries several signatures; the rewritten bytes need
    // exactly the same key set, otherwise a declared attachment goes stale.
    let primary_id = sign::key_id(&args.key)?;
    let mut extra_keys: Vec<(String, &std::path::PathBuf)> = Vec::new();
    for path in &args.additional_signing_keys {
        if !path.is_file() {
            bail!("--additional-signing-key is not a file: {}", path.display());
        }
        let id = sign::key_id(path)?;
        if id == primary_id {
            bail!("--additional-signing-key duplicates the primary --key");
        }
        if extra_keys.iter().any(|(existing, _)| *existing == id) {
            bail!("--additional-signing-key repeated the same key id: {id}");
        }
        extra_keys.push((id, path));
    }
    let mut declared = parsed.signing_key_ids.clone();
    declared.sort();
    let mut provided: Vec<String> = extra_keys.iter().map(|(id, _)| id.clone()).collect();
    provided.sort();
    if declared != provided {
        bail!(
            "manifest declares signing_key_ids {declared:?} but --additional-signing-key provides {provided:?}; pass the same key set so rotation stays intact"
        );
    }

    if args.verify {
        let mut trusted = vec![public_key.clone()];
        for (_, path) in &extra_keys {
            trusted.push(sign::public_key_base64(path)?);
        }
        let report = audit::audit_release(publisher.as_ref(), &prefix, &args.version, &trusted)?;
        if !report.missing.is_empty() {
            for item in &report.missing {
                output::warn(item);
            }
            bail!(
                "{} object(s) are missing or have the wrong size; refusing to promote",
                report.missing.len()
            );
        }
    }

    let mut value: serde_json::Value =
        serde_json::from_slice(&raw).context("parse the published manifest")?;
    value["channel"] = serde_json::Value::String(args.channel.clone());
    let manifest = serde_json::to_vec_pretty(&value).context("serialize promoted manifest")?;
    let signature = sign::sign_manifest(&manifest, &args.key)?;
    let attachments: Vec<(String, Vec<u8>)> = extra_keys
        .iter()
        .map(|(id, path)| Ok((id.clone(), sign::sign_manifest(&manifest, path)?.bytes)))
        .collect::<anyhow::Result<_>>()?;
    let sbom = sbom::build_sbom(&manifest, &args.channel, &args.version)?;

    if args.dry_run {
        report(
            args,
            &previous_channel,
            manifest.len(),
            1 + attachments.len(),
            true,
        );
        return Ok(());
    }
    publish::publish_manifests(
        publisher.as_ref(),
        &publish::ManifestSet {
            prefix: &prefix,
            version: &args.version,
            manifest: &manifest,
            signature: &signature.bytes,
            attachments: &attachments,
        },
    )?;
    publish::publish_sbom(publisher.as_ref(), &prefix, &args.version, &sbom)?;
    report(
        args,
        &previous_channel,
        manifest.len(),
        1 + attachments.len(),
        false,
    );
    Ok(())
}

fn report(args: &PromoteArgs, previous: &str, bytes: usize, signatures: usize, dry_run: bool) {
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "v": 1,
                "tool": "delta-update-cli",
                "version": args.version,
                "from_channel": previous,
                "channel": args.channel,
                "manifest_bytes": bytes,
                "signatures": signatures,
                "dry_run": dry_run,
            }))
            .unwrap_or_else(|_| "{}".into())
        );
        return;
    }
    if dry_run {
        output::info(&format!(
            "dry-run promote: {} {previous} -> {} ({bytes} byte manifest, {signatures} signature(s))",
            args.version, args.channel
        ));
    } else {
        output::info(&format!(
            "promoted {}: {previous} -> {}; latest.json now points at it",
            args.version, args.channel
        ));
    }
}
