use std::path::PathBuf;

use delta_update_cli::cli::{InspectArgs, ReleaseArgs, RollbackArgs, VerifyArgs};
use delta_update_cli::pipeline;
use delta_update_cli::publisher::fs::FsPublisher;
use delta_update_cli::publisher::Publisher;
use delta_update_engine::limits::Limits;
use delta_update_engine::manifest::parse_manifest;
use serde_json::Value;

struct Fixture {
    _temp: tempfile::TempDir,
    from: PathBuf,
    to: PathBuf,
    key: PathBuf,
    public_key: PathBuf,
    publish: PathBuf,
    public_base64: String,
}

fn fixture() -> Fixture {
    std::env::set_var("MINISIGN_PASSWORD", "");
    let temp = tempfile::tempdir().unwrap();
    let from = temp.path().join("from");
    let to = temp.path().join("to");
    std::fs::create_dir_all(from.join("bin")).unwrap();
    std::fs::create_dir_all(to.join("bin")).unwrap();
    std::fs::create_dir_all(from.join("resources")).unwrap();
    std::fs::create_dir_all(to.join("resources")).unwrap();

    let mut old = b"version one payload".to_vec();
    old.extend(std::iter::repeat_n(b'a', 8 * 1024));
    let mut new = old.clone();
    new[20..30].copy_from_slice(b"changed123");
    new.extend(std::iter::repeat_n(b'b', 512));
    std::fs::write(from.join("bin/app.exe"), &old).unwrap();
    std::fs::write(to.join("bin/app.exe"), &new).unwrap();
    std::fs::write(from.join("resources/icon.png"), b"png-old").unwrap();
    std::fs::write(to.join("resources/icon.png"), b"png-new").unwrap();
    std::fs::write(to.join("added.txt"), b"brand new file").unwrap();
    std::fs::write(from.join("same.txt"), b"unchanged").unwrap();
    std::fs::write(to.join("same.txt"), b"unchanged").unwrap();
    std::fs::write(from.join("removed.txt"), b"gone").unwrap();

    let keypair = minisign::KeyPair::generate_encrypted_keypair(Some(String::new())).unwrap();
    let key = temp.path().join("release.key");
    let public_key = temp.path().join("release.pub");
    std::fs::write(&key, keypair.sk.to_box(None).unwrap().to_bytes()).unwrap();
    std::fs::write(&public_key, keypair.pk.to_box().unwrap().to_bytes()).unwrap();

    Fixture {
        publish: temp.path().join("publish"),
        _temp: temp,
        from,
        to,
        key,
        public_key,
        public_base64: keypair.pk.to_base64(),
    }
}

fn release_args(
    fixture: &Fixture,
    version: &str,
    from_version: Option<&str>,
    rollback: bool,
    dry_run: bool,
) -> ReleaseArgs {
    ReleaseArgs {
        to: fixture.to.clone(),
        version: version.to_string(),
        bucket: fixture.publish.display().to_string(),
        key: fixture.key.clone(),
        additional_signing_keys: Vec::new(),
        from: Some(fixture.from.clone()),
        from_version: from_version.map(str::to_string),
        initial: false,
        rollback,
        channel: "stable".into(),
        notes_file: None,
        mandatory: false,
        min_supported: None,
        region: None,
        prefix: None,
        dry_run,
        require_delta: false,
        delta_max_ratio: 0.60,
        patch_max_file_size: 64 * 1024 * 1024,
        verify: false,
        self_test: false,
        pipeline: false,
        report: "release-report.json".into(),
        watch: None,
        interval: 60,
        jobs: Some(2),
        verbose: false,
    }
}

fn publish_previous_source(fixture: &Fixture) {
    let publisher = FsPublisher::new(&fixture.publish);
    for path in ["bin/app.exe", "resources/icon.png"] {
        let source = fixture.from.join(path);
        let hash = delta_update_engine::crypto::sha256_file(&source).unwrap();
        publisher
            .put_file(&format!("blobs/{hash}"), &source)
            .unwrap();
    }
}

#[test]
fn self_test_applies_and_rolls_back_the_generated_update() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, true);
    args.self_test = true;
    pipeline::release(&args).unwrap();
}

#[test]
fn release_builds_a_verifiable_manifest_and_applies_sampled_patches() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    pipeline::release(&release_args(
        &fixture,
        "2.0.0",
        Some("1.0.0"),
        false,
        false,
    ))
    .unwrap();

    let publisher = FsPublisher::new(&fixture.publish);
    let manifest_bytes = publisher.get("manifests/latest.json").unwrap();
    let signature = publisher.get("manifests/latest.json.minisig").unwrap();
    delta_update_engine::crypto::verify_manifest(
        &manifest_bytes,
        std::str::from_utf8(&signature).unwrap(),
        std::slice::from_ref(&fixture.public_base64),
    )
    .unwrap();
    assert_eq!(
        manifest_bytes,
        publisher.get("manifests/2.0.0.json").unwrap()
    );

    let manifest = parse_manifest(&manifest_bytes, &Limits::default()).unwrap();
    assert_eq!(manifest.version, "2.0.0");
    assert_eq!(manifest.channel.as_deref(), Some("stable"));
    assert_eq!(manifest.files.len(), 4);
    assert!(!manifest.files.contains_key("removed.txt"));

    let app = &manifest.files["bin/app.exe"];
    assert_eq!(app.strategy.as_str(), "qbsdiff+zstd");
    assert!(app.delta_key.is_some());
    assert!(app.full_key.as_deref().unwrap().starts_with("blobs/"));

    assert_eq!(
        manifest.files["resources/icon.png"].strategy.as_str(),
        "full"
    );
    assert!(manifest.files["resources/icon.png"].delta_key.is_none());
    assert!(manifest.files["added.txt"].full_key.is_some());
    assert!(manifest.files["same.txt"].full_key.is_some());
    assert!(manifest.files["same.txt"].delta_key.is_none());

    let verify_args = VerifyArgs {
        bucket: fixture.publish.display().to_string(),
        version: "2.0.0".into(),
        public_key: fixture.public_key.clone(),
        sample: 3,
        region: None,
        prefix: None,
    };
    pipeline::verify(&verify_args).unwrap();
}

#[test]
fn release_writes_an_advisory_sbom() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    pipeline::release(&release_args(
        &fixture,
        "2.0.0",
        Some("1.0.0"),
        false,
        false,
    ))
    .unwrap();

    let publisher = FsPublisher::new(&fixture.publish);
    let sbom: Value =
        serde_json::from_slice(&publisher.get("manifests/2.0.0.sbom.json").unwrap()).unwrap();
    assert_eq!(sbom["format"], "delta-update-sbom");
    assert_eq!(sbom["version"], "2.0.0");
    let paths: Vec<&str> = sbom["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["added.txt", "bin/app.exe", "resources/icon.png", "same.txt"]
    );
}

#[test]
fn release_embeds_the_min_supported_version() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, false);
    args.min_supported = Some("1.5.0".into());
    pipeline::release(&args).unwrap();

    let publisher = FsPublisher::new(&fixture.publish);
    let manifest = parse_manifest(
        &publisher.get("manifests/latest.json").unwrap(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(manifest.min_supported_version.as_deref(), Some("1.5.0"));
}

#[test]
fn release_rejects_a_min_supported_version_above_the_release() {
    let fixture = fixture();
    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, true);
    args.min_supported = Some("2.1.0".into());
    let error = pipeline::release(&args).unwrap_err();
    assert!(error.to_string().contains("must not exceed"));
}

#[test]
fn pipeline_self_tests_uploads_audits_and_writes_a_report() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    // Pre-publish the unchanged file's content-addressed blob: this release
    // must count it as reused instead of uploading it again.
    let unchanged = fixture.to.join("same.txt");
    let unchanged_hash = delta_update_engine::crypto::sha256_file(&unchanged).unwrap();
    FsPublisher::new(&fixture.publish)
        .put_file(&format!("blobs/{unchanged_hash}"), &unchanged)
        .unwrap();
    let report_path = fixture._temp.path().join("release-report.json");
    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, false);
    args.pipeline = true;
    args.report = report_path.clone();
    pipeline::release(&args).unwrap();

    let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["v"], 1);
    assert_eq!(report["tool"], "delta-update-cli");
    assert_eq!(report["version"], "2.0.0");
    assert_eq!(report["self_test"]["ran"], true);
    assert_eq!(report["audit"]["ok"], true);
    assert_eq!(report["audit"]["missing_objects"], serde_json::json!([]));
    assert!(report["coverage"]["modified"].as_u64().unwrap() >= 1);

    let changed = report["changed_bytes"].as_u64().unwrap();
    let delta = report["delta_bytes"].as_u64().unwrap();
    let patched_full = changed - report["client"]["full_download_bytes"].as_u64().unwrap();
    let app = fixture.to.join("bin/app.exe").metadata().unwrap().len();
    let icon = fixture
        .to
        .join("resources/icon.png")
        .metadata()
        .unwrap()
        .len();
    let added = fixture.to.join("added.txt").metadata().unwrap().len();
    assert_eq!(
        changed,
        app + icon + added,
        "unchanged files are not part of changed_bytes"
    );
    assert_eq!(
        report["client"]["full_download_bytes"].as_u64().unwrap(),
        icon + added,
        "a modified file that fell back to full must not count as patched"
    );
    assert_eq!(report["release_kind"], "incremental");
    assert_eq!(
        report["client"]["net_savings_bytes"].as_u64().unwrap(),
        patched_full - delta,
        "net savings must be measured against the patched files, not all changed files"
    );
    assert_eq!(
        patched_full, app,
        "only the file that actually got a delta contributes to the baseline"
    );
    assert!(report["client"]["net_savings_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        report["publish"]["uploaded_delta_bytes"],
        report["delta_bytes"]
    );
    assert_eq!(
        report["publish"]["reused_full_bytes"].as_u64().unwrap(),
        unchanged.metadata().unwrap().len()
    );
    assert_eq!(report["publish"]["reused_full_blobs"], 1);
    assert!(report["publish"]["uploaded_full_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn release_publishes_additional_signatures_and_lists_them() {
    let fixture = fixture();
    publish_previous_source(&fixture);

    let extra_dir = tempfile::tempdir().unwrap();
    let extra = minisign::KeyPair::generate_encrypted_keypair(Some(String::new())).unwrap();
    let extra_key = extra_dir.path().join("extra.key");
    std::fs::write(&extra_key, extra.sk.to_box(None).unwrap().to_bytes()).unwrap();
    let extra_id = hex::encode(extra.pk.keynum());

    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, false);
    args.additional_signing_keys = vec![extra_key];
    pipeline::release(&args).unwrap();

    let publisher = FsPublisher::new(&fixture.publish);
    let manifest = publisher.get("manifests/latest.json").unwrap();
    let parsed: Value = serde_json::from_slice(&manifest).unwrap();
    assert_eq!(
        parsed["signing_key_ids"],
        serde_json::json!([extra_id.clone()])
    );

    let attachment = publisher
        .get(&format!("manifests/latest.json.minisig.{extra_id}"))
        .unwrap();
    delta_update_engine::crypto::verify_manifest(
        &manifest,
        std::str::from_utf8(&attachment).unwrap(),
        &[extra.pk.to_base64()],
    )
    .unwrap();

    let primary = publisher.get("manifests/latest.json.minisig").unwrap();
    delta_update_engine::crypto::verify_manifest(
        &manifest,
        std::str::from_utf8(&primary).unwrap(),
        std::slice::from_ref(&fixture.public_base64),
    )
    .unwrap();
}

#[test]
fn additional_signing_key_must_differ_from_the_primary() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, true);
    args.additional_signing_keys = vec![fixture.key.clone()];
    let error = pipeline::release(&args).unwrap_err();
    assert!(error.to_string().contains("duplicates the primary"));
}

#[test]
fn rollback_release_is_reported_as_its_own_kind() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    pipeline::release(&release_args(
        &fixture,
        "2.0.0",
        Some("1.0.0"),
        false,
        false,
    ))
    .unwrap();

    let report_path = fixture._temp.path().join("rollback-report.json");
    let mut args = release_args(&fixture, "1.5.0", Some("2.0.0"), true, false);
    args.pipeline = true;
    args.report = report_path.clone();
    pipeline::release(&args).unwrap();

    let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["release_kind"], "rollback");
}

#[test]
fn pipeline_watch_reaudits_the_bucket_and_records_rounds() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    let report_path = fixture._temp.path().join("release-report.json");
    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, false);
    args.pipeline = true;
    args.report = report_path.clone();
    args.watch = Some(1);
    args.interval = 1;
    pipeline::release(&args).unwrap();

    let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["watch"]["seconds"], 1);
    assert!(report["watch"]["rounds"].as_u64().unwrap() >= 1);
}

#[test]
fn pipeline_rejects_dry_run() {
    let fixture = fixture();
    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, true);
    args.pipeline = true;
    let error = pipeline::release(&args).unwrap_err();
    assert!(error.to_string().contains("remove --dry-run"));
}

#[test]
fn pipeline_skips_self_test_for_an_initial_release() {
    let fixture = fixture();
    let report_path = fixture._temp.path().join("release-report.json");
    let mut args = release_args(&fixture, "2.0.0", None, false, false);
    args.initial = true;
    args.from = None;
    args.pipeline = true;
    args.report = report_path.clone();
    pipeline::release(&args).unwrap();

    let report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["self_test"]["ran"], false);
    assert_eq!(report["self_test"]["reason"], "initial release");
    assert_eq!(report["audit"]["ok"], true);
    assert_eq!(report["release_kind"], "initial");
    assert_eq!(report["client"]["net_savings_bytes"], 0);
    assert_eq!(report["publish"]["reused_full_blobs"], 0);
}

fn rollback_args(fixture: &Fixture, to_version: &str) -> RollbackArgs {
    RollbackArgs {
        bucket: fixture.publish.display().to_string(),
        to_version: to_version.to_string(),
        key: fixture.key.clone(),
        region: None,
        prefix: None,
        dry_run: false,
    }
}

#[test]
fn rollback_points_latest_at_an_existing_manifest_without_rebuilding() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    pipeline::release(&release_args(
        &fixture,
        "1.9.0",
        Some("1.0.0"),
        false,
        false,
    ))
    .unwrap();
    pipeline::release(&release_args(
        &fixture,
        "2.0.0",
        Some("1.9.0"),
        false,
        false,
    ))
    .unwrap();

    let publisher = FsPublisher::new(&fixture.publish);
    let original = publisher.get("manifests/1.9.0.json").unwrap();
    pipeline::rollback(&rollback_args(&fixture, "1.9.0")).unwrap();

    assert_eq!(
        publisher.get("manifests/1.9.0.json").unwrap(),
        original,
        "the versioned manifest must stay byte-identical"
    );
    let latest = publisher.get("manifests/latest.json").unwrap();
    let value: Value = serde_json::from_slice(&latest).unwrap();
    assert_eq!(value["version"], "1.9.0");
    assert_eq!(value["rollback"], true);
    let signature = publisher.get("manifests/latest.json.minisig").unwrap();
    delta_update_engine::crypto::verify_manifest(
        &latest,
        std::str::from_utf8(&signature).unwrap(),
        std::slice::from_ref(&fixture.public_base64),
    )
    .unwrap();
}

#[test]
fn rollback_dry_run_does_not_touch_latest() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    pipeline::release(&release_args(
        &fixture,
        "1.9.0",
        Some("1.0.0"),
        false,
        false,
    ))
    .unwrap();
    pipeline::release(&release_args(
        &fixture,
        "2.0.0",
        Some("1.9.0"),
        false,
        false,
    ))
    .unwrap();
    let publisher = FsPublisher::new(&fixture.publish);
    let before = publisher.get("manifests/latest.json").unwrap();

    let mut args = rollback_args(&fixture, "1.9.0");
    args.dry_run = true;
    pipeline::rollback(&args).unwrap();

    assert_eq!(publisher.get("manifests/latest.json").unwrap(), before);
}

#[test]
fn rollback_rejects_a_key_that_did_not_sign_the_manifest() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    pipeline::release(&release_args(
        &fixture,
        "1.9.0",
        Some("1.0.0"),
        false,
        false,
    ))
    .unwrap();

    let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let other_path = fixture._temp.path().join("other.key");
    std::fs::write(&other_path, other.sk.to_box(None).unwrap().to_bytes()).unwrap();
    let mut args = rollback_args(&fixture, "1.9.0");
    args.key = other_path;
    assert!(pipeline::rollback(&args).is_err());
}

#[test]
fn rollback_manifest_is_flagged_and_allowed() {
    let fixture = fixture();
    pipeline::release(&release_args(&fixture, "1.5.0", Some("2.0.0"), true, false)).unwrap();

    let publisher = FsPublisher::new(&fixture.publish);
    let bytes = publisher.get("manifests/latest.json").unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["rollback"], Value::Bool(true));
    assert_eq!(value["version"], Value::String("1.5.0".into()));
}

#[test]
fn dry_run_does_not_open_or_write_the_bucket() {
    let fixture = fixture();
    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, true);
    args.bucket = "cos://not-a-real-bucket".into();
    pipeline::release(&args).unwrap();
    assert!(!fixture.publish.exists());
}

#[test]
fn release_rejects_a_non_increasing_version() {
    let fixture = fixture();
    let error = pipeline::release(&release_args(&fixture, "1.0.0", Some("1.0.0"), false, true))
        .unwrap_err();
    assert!(error.to_string().contains("must be greater"));
}

#[test]
fn require_delta_fails_when_a_patch_falls_back() {
    let fixture = fixture();
    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, true);
    args.require_delta = true;
    args.delta_max_ratio = 0.0001;
    let error = pipeline::release(&args).unwrap_err();
    assert!(error.to_string().contains("--require-delta"));
}

#[test]
fn prefix_is_applied_to_objects_and_manifests() {
    let fixture = fixture();
    let publisher = FsPublisher::new(&fixture.publish);
    let source = fixture.from.join("bin/app.exe");
    let source_hash = delta_update_engine::crypto::sha256_file(&source).unwrap();
    publisher
        .put_file(&format!("releases/blobs/{source_hash}"), &source)
        .unwrap();

    let mut args = release_args(&fixture, "2.0.0", Some("1.0.0"), false, false);
    args.prefix = Some("releases".into());
    pipeline::release(&args).unwrap();

    let publisher = FsPublisher::new(&fixture.publish);
    assert!(publisher.head("manifests/latest.json").unwrap().is_none());
    let bytes = publisher.get("releases/manifests/latest.json").unwrap();
    let manifest = parse_manifest(&bytes, &Limits::default()).unwrap();
    let app = &manifest.files["bin/app.exe"];
    assert!(app
        .full_key
        .as_deref()
        .unwrap()
        .starts_with("releases/blobs/"));
    assert!(app
        .delta_key
        .as_deref()
        .unwrap()
        .starts_with("releases/artifacts/1.0.0-to-2.0.0/"));

    let verify_args = VerifyArgs {
        bucket: fixture.publish.display().to_string(),
        version: "2.0.0".into(),
        public_key: fixture.public_key.clone(),
        sample: 1,
        region: None,
        prefix: Some("releases".into()),
    };
    pipeline::verify(&verify_args).unwrap();
}

#[test]
fn verify_rejects_a_signature_from_another_key() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    pipeline::release(&release_args(
        &fixture,
        "2.0.0",
        Some("1.0.0"),
        false,
        false,
    ))
    .unwrap();

    let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let other_path = fixture._temp.path().join("other.pub");
    std::fs::write(&other_path, other.pk.to_box().unwrap().to_bytes()).unwrap();
    let verify_args = VerifyArgs {
        bucket: fixture.publish.display().to_string(),
        version: "2.0.0".into(),
        public_key: other_path,
        sample: 1,
        region: None,
        prefix: None,
    };
    assert!(pipeline::verify(&verify_args).is_err());
}

#[test]
fn verify_key_detects_matching_and_mismatched_pairs() {
    let fixture = fixture();
    let args = delta_update_cli::cli::VerifyKeyArgs {
        key: fixture.key.clone(),
        public: fixture.public_base64.clone(),
        json: false,
    };
    pipeline::verify_key(&args).unwrap();

    let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let args = delta_update_cli::cli::VerifyKeyArgs {
        key: fixture.key,
        public: other.pk.to_base64(),
        json: false,
    };
    assert!(pipeline::verify_key(&args).is_err());
}

fn inspect_args(fixture: &Fixture) -> InspectArgs {
    InspectArgs {
        journal_dir: None,
        bundle: None,
        from_release: Some(fixture.publish.display().to_string()),
        version: Some("2.0.0".into()),
        public_key: Some(fixture.public_key.clone()),
        prefix: None,
        verbose: false,
        json: false,
        export: None,
        redact: false,
    }
}

#[test]
fn inspect_from_release_audits_a_complete_release() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    pipeline::release(&release_args(
        &fixture,
        "2.0.0",
        Some("1.0.0"),
        false,
        false,
    ))
    .unwrap();

    pipeline::inspect_release(&inspect_args(&fixture)).unwrap();
}

#[test]
fn inspect_from_release_reports_missing_objects() {
    let fixture = fixture();
    publish_previous_source(&fixture);
    pipeline::release(&release_args(
        &fixture,
        "2.0.0",
        Some("1.0.0"),
        false,
        false,
    ))
    .unwrap();

    let publisher = FsPublisher::new(&fixture.publish);
    let manifest = parse_manifest(
        &publisher.get("manifests/2.0.0.json").unwrap(),
        &Limits::default(),
    )
    .unwrap();
    let key = manifest.files["bin/app.exe"].full_key.clone().unwrap();
    std::fs::remove_file(fixture.publish.join(&key)).unwrap();

    let error = pipeline::inspect_release(&inspect_args(&fixture)).unwrap_err();
    assert!(error.to_string().contains("missing"));
}
