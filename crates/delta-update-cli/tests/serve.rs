use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;

use delta_update_cli::commands::serve::serve_on;

fn start(root: &Path) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    std::thread::spawn(move || serve_on(listener, root));
    address
}

fn raw(address: SocketAddr, request: &str) -> String {
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    response
}

fn get(address: SocketAddr, target: &str) -> String {
    raw(
        address,
        &format!("GET {target} HTTP/1.1\r\nHost: test\r\n\r\n"),
    )
}

fn ranged(address: SocketAddr, target: &str, range: &str) -> String {
    raw(
        address,
        &format!("GET {target} HTTP/1.1\r\nHost: test\r\nRange: {range}\r\n\r\n"),
    )
}

#[test]
fn serves_a_file_and_honours_head() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("manifests")).unwrap();
    std::fs::write(dir.path().join("manifests/latest.json"), b"{}").unwrap();
    let address = start(dir.path());

    let response = get(address, "/manifests/latest.json");
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("Content-Length: 2"), "{response}");
    assert!(response.ends_with("{}"), "{response}");

    let head = raw(
        address,
        "HEAD /manifests/latest.json HTTP/1.1\r\nHost: test\r\n\r\n",
    );
    assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
    assert!(head.contains("Content-Length: 2"), "{head}");
    assert!(
        head.ends_with("\r\n\r\n"),
        "HEAD must not return a body: {head}"
    );
}

#[test]
fn serves_single_ranges_and_rejects_out_of_bounds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("data.bin"), b"0123456789").unwrap();
    let address = start(dir.path());

    let partial = ranged(address, "/data.bin", "bytes=2-4");
    assert!(partial.starts_with("HTTP/1.1 206"), "{partial}");
    assert!(partial.contains("Content-Range: bytes 2-4/10"), "{partial}");
    assert!(partial.ends_with("234"), "{partial}");

    let open_ended = ranged(address, "/data.bin", "bytes=7-");
    assert!(open_ended.starts_with("HTTP/1.1 206"), "{open_ended}");
    assert!(open_ended.ends_with("789"), "{open_ended}");

    let clamped = ranged(address, "/data.bin", "bytes=8-99");
    assert!(clamped.starts_with("HTTP/1.1 206"), "{clamped}");
    assert!(clamped.contains("Content-Range: bytes 8-9/10"), "{clamped}");

    let out_of_bounds = ranged(address, "/data.bin", "bytes=99-");
    assert!(out_of_bounds.starts_with("HTTP/1.1 416"), "{out_of_bounds}");
    assert!(
        out_of_bounds.contains("Content-Range: bytes */10"),
        "{out_of_bounds}"
    );

    // Multiple ranges are ignored; a full 200 keeps the engine's fallback path
    // deterministic instead of silently serving only the first range.
    let multi = ranged(address, "/data.bin", "bytes=0-1,3-4");
    assert!(multi.starts_with("HTTP/1.1 200 OK"), "{multi}");
    assert!(multi.ends_with("0123456789"), "{multi}");
}

#[test]
fn rejects_traversal_forms_and_directories() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("manifests")).unwrap();
    std::fs::write(dir.path().join("manifests/latest.json"), b"{}").unwrap();
    let name = format!("delta-serve-secret-{}.txt", std::process::id());
    let secret = dir.path().parent().unwrap().join(&name);
    std::fs::write(&secret, b"must-not-leak").unwrap();
    let address = start(dir.path());

    let targets = [
        format!("/../{name}"),
        format!("/%2e%2e%2f{name}"),
        format!("/..%5C{name}"),
        format!("/..\\{name}"),
        "/C:/Windows/win.ini".to_string(),
        "/manifests".to_string(),
        "/".to_string(),
    ];
    for target in &targets {
        let response = get(address, target);
        assert!(
            response.starts_with("HTTP/1.1 404"),
            "{target} must not be served: {response}"
        );
        assert!(!response.contains("must-not-leak"), "{target}: {response}");
    }

    let _ = std::fs::remove_file(secret);
}

#[test]
fn decodes_percent_encoded_file_names() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a b.txt"), b"ok").unwrap();
    let address = start(dir.path());

    let response = get(address, "/a%20b.txt");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("ok"), "{response}");
}

#[cfg(windows)]
#[test]
fn rejects_symlinks_that_escape_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"must-not-leak").unwrap();
    let link = dir.path().join("link.txt");
    if std::os::windows::fs::symlink_file(outside.path().join("secret.txt"), &link).is_err() {
        // Symlink creation needs Developer Mode or elevation; skip when unavailable.
        return;
    }
    let address = start(dir.path());
    let response = get(address, "/link.txt");
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
}
