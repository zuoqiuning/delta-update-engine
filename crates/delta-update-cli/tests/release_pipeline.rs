use std::path::PathBuf;

use delta_update_cli::cli::{ReleaseArgs, VerifyArgs};
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
        from: Some(fixture.from.clone()),
        from_version: from_version.map(str::to_string),
        initial: false,
        rollback,
        channel: "stable".into(),
        notes_file: None,
        mandatory: false,
        region: None,
        prefix: None,
        dry_run,
        require_delta: false,
        delta_max_ratio: 0.60,
        patch_max_file_size: 64 * 1024 * 1024,
        verify: false,
        jobs: Some(2),
        verbose: false,
    }
}

fn publish_previous_source(fixture: &Fixture) {
    let publisher = FsPublisher::new(&fixture.publish);
    for path in ["bin/app.exe", "resources/icon.png"] {
        publisher
            .put_file(
                &format!("artifacts/full/1.0.0/{path}"),
                &fixture.from.join(path),
            )
            .unwrap();
    }
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
    assert!(app.full_key.is_some());

    assert_eq!(
        manifest.files["resources/icon.png"].strategy.as_str(),
        "full"
    );
    assert!(manifest.files["resources/icon.png"].delta_key.is_none());
    assert!(manifest.files["added.txt"].full_key.is_some());
    assert!(manifest.files["same.txt"].full_key.is_none());

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
    publisher
        .put_file(
            "releases/artifacts/full/1.0.0/bin/app.exe",
            &fixture.from.join("bin/app.exe"),
        )
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
        .starts_with("releases/artifacts/full/2.0.0/"));
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
    };
    pipeline::verify_key(&args).unwrap();

    let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let args = delta_update_cli::cli::VerifyKeyArgs {
        key: fixture.key,
        public: other.pk.to_base64(),
    };
    assert!(pipeline::verify_key(&args).is_err());
}
