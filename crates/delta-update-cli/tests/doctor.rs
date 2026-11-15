use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use delta_update_cli::cli;
use delta_update_cli::pipeline;
use delta_update_cli::pipeline::manifest::{self, ManifestFile, ManifestInput};
use delta_update_cli::pipeline::sign;
use delta_update_engine::crypto::sha256_bytes;

struct StaticServer {
    endpoint: String,
    handle: JoinHandle<()>,
}

fn serve(files: HashMap<String, Vec<u8>>, requests: usize) -> StaticServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        for _ in 0..requests {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0_u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]);
            let mut parts = request.lines().next().unwrap_or("").split_whitespace();
            let method = parts.next().unwrap_or("");
            let path = parts.next().unwrap_or("");
            let response = match files.get(path) {
                Some(body) => {
                    let body = if method == "HEAD" {
                        &[][..]
                    } else {
                        body.as_slice()
                    };
                    let mut response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        files[path].len()
                    )
                    .into_bytes();
                    response.extend_from_slice(body);
                    response
                }
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec(),
            };
            let _ = stream.write_all(&response);
        }
    });
    StaticServer {
        endpoint: format!("http://127.0.0.1:{port}/manifests/latest.json"),
        handle,
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    files: HashMap<String, Vec<u8>>,
    public_key: PathBuf,
}

fn fixture() -> Fixture {
    std::env::set_var("MINISIGN_PASSWORD", "");
    let temp = tempfile::tempdir().unwrap();
    let keypair = minisign::KeyPair::generate_encrypted_keypair(Some(String::new())).unwrap();
    let key_path = temp.path().join("release.key");
    let public_path = temp.path().join("release.pub");
    std::fs::write(&key_path, keypair.sk.to_box(None).unwrap().to_bytes()).unwrap();
    std::fs::write(&public_path, keypair.pk.to_box().unwrap().to_bytes()).unwrap();

    let payload = b"hello doctor";
    let manifest = manifest::build(ManifestInput {
        version: "2.0.0",
        channel: "stable",
        rollback: false,
        notes: None,
        mandatory: false,
        min_supported_version: None,
        signing_key_ids: &[],
        files: std::collections::BTreeMap::from([(
            "a.txt".to_string(),
            ManifestFile {
                sha256: sha256_bytes(payload),
                size: payload.len() as u64,
                strategy: "full".into(),
                delta_key: None,
                delta_size: None,
                full_key: Some("artifacts/full/2.0.0/a.txt".into()),
            },
        )]),
    })
    .unwrap();
    let signature = sign::sign_manifest(&manifest, &key_path).unwrap();

    let mut files = HashMap::new();
    files.insert("/manifests/latest.json".to_string(), manifest);
    files.insert(
        "/manifests/latest.json.minisig".to_string(),
        signature.bytes,
    );
    files.insert("/artifacts/full/2.0.0/a.txt".to_string(), payload.to_vec());
    Fixture {
        _temp: temp,
        files,
        public_key: public_path,
    }
}

fn doctor_args(endpoint: &str, public_key: &Path) -> cli::DoctorArgs {
    cli::DoctorArgs {
        endpoint: vec![endpoint.to_string()],
        public_key: Some(public_key.to_path_buf()),
        check_config: false,
        simulate_failure: None,
        reason: None,
        dir: ".".into(),
        watch: None,
        interval: 60,
        json: false,
    }
}

#[test]
fn doctor_verifies_signature_and_objects() {
    let fixture = fixture();
    let server = serve(fixture.files, 3);
    pipeline::doctor(&doctor_args(&server.endpoint, &fixture.public_key)).unwrap();
    server.handle.join().unwrap();
}

#[test]
fn doctor_reports_missing_objects() {
    let mut fixture = fixture();
    fixture.files.remove("/artifacts/full/2.0.0/a.txt");
    let server = serve(fixture.files, 3);
    assert!(pipeline::doctor(&doctor_args(&server.endpoint, &fixture.public_key)).is_err());
    server.handle.join().unwrap();
}

#[test]
fn doctor_rejects_presigned_endpoints() {
    let fixture = fixture();
    let args = doctor_args(
        "http://127.0.0.1:1/manifests/latest.json?sign=abc",
        &fixture.public_key,
    );
    assert!(
        pipeline::doctor(&args).is_err(),
        "presigned endpoints are rejected on purpose"
    );
}

#[test]
fn doctor_json_shape_is_stable() {
    let fixture = fixture();
    let server = serve(fixture.files, 3);
    let mut args = doctor_args(&server.endpoint, &fixture.public_key);
    args.json = true;
    pipeline::doctor(&args).unwrap();
    server.handle.join().unwrap();
}

#[test]
fn doctor_watch_zero_runs_a_single_pass() {
    let fixture = fixture();
    let server = serve(fixture.files, 3);
    let mut args = doctor_args(&server.endpoint, &fixture.public_key);
    args.watch = Some(0);
    pipeline::doctor(&args).unwrap();
    server.handle.join().unwrap();
}

#[test]
fn doctor_watch_rejects_json_output() {
    let fixture = fixture();
    let mut args = doctor_args(
        "https://example.com/manifests/latest.json",
        &fixture.public_key,
    );
    args.watch = Some(60);
    args.json = true;
    let error = pipeline::doctor(&args).unwrap_err();
    assert!(error.to_string().contains("--json"));
}

fn config_project(endpoint: &str) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("src-tauri")).unwrap();
    std::fs::write(
        temp.path().join("src-tauri/tauri.conf.json"),
        format!(
            r#"{{
                "plugins": {{
                    "delta-update": {{
                        "config_version": 1,
                        "endpoint": "{endpoint}",
                        "signing_public_keys": ["RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"]
                    }}
                }}
            }}"#
        ),
    )
    .unwrap();
    temp
}

fn check_config_args(dir: &Path) -> cli::DoctorArgs {
    cli::DoctorArgs {
        endpoint: Vec::new(),
        public_key: None,
        check_config: true,
        simulate_failure: None,
        reason: None,
        dir: dir.to_path_buf(),
        watch: None,
        interval: 60,
        json: false,
    }
}

#[test]
fn doctor_check_config_accepts_a_valid_plugin_section() {
    let project = config_project("https://example.com/manifests/latest.json");
    pipeline::doctor(&check_config_args(project.path())).unwrap();
}

#[test]
fn doctor_check_config_rejects_an_insecure_endpoint() {
    let project = config_project("http://example.com/manifests/latest.json");
    let error = pipeline::doctor(&check_config_args(project.path())).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<delta_update_engine::EngineError>()
            .map(delta_update_engine::EngineError::code),
        Some("E_ENDPOINT_INSECURE")
    );
}

#[test]
fn simulated_failures_carry_the_codes_the_client_matches_on() {
    for (mode, code) in [
        (cli::SimulateMode::Lock, "E_UPDATE_LOCKED"),
        (cli::SimulateMode::Patch, "E_PATCH_SOURCE_MISMATCH"),
        (cli::SimulateMode::Disk, "E_DISK_INSUFFICIENT"),
    ] {
        assert_eq!(
            pipeline::simulated_error(mode, None).unwrap().code(),
            code,
            "{mode:?}"
        );
    }
}

#[test]
fn simulated_patch_reasons_cover_every_recovery_branch() {
    for (reason, code) in [
        (cli::PatchReason::SourceMismatch, "E_PATCH_SOURCE_MISMATCH"),
        (cli::PatchReason::Format, "E_PATCH_FORMAT_MISMATCH"),
        (cli::PatchReason::TargetHash, "E_PATCH_TARGET_HASH_MISMATCH"),
        (cli::PatchReason::Memory, "E_PATCH_MEMORY_BUDGET"),
    ] {
        assert_eq!(
            pipeline::simulated_error(cli::SimulateMode::Patch, Some(reason))
                .unwrap()
                .code(),
            code,
            "{reason:?}"
        );
    }
}
