//! Generate a disposable minisign key pair for the real test harness.
//!
//! The secret key is written in the same format `delta-update release --key`
//! expects. These keys are test-only; never use them for a release.

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let secret_path = args.next().map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("usage: testkeygen <secret.key> <public.pub>");
        std::process::exit(2);
    });
    let public_path = args.next().map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("usage: testkeygen <secret.key> <public.pub>");
        std::process::exit(2);
    });

    let keypair = minisign::KeyPair::generate_encrypted_keypair(Some(String::new()))
        .expect("generate minisign key pair");
    let secret = keypair
        .sk
        .to_box(None)
        .expect("serialize secret key")
        .to_bytes();

    for path in [&secret_path, &public_path] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create key directory");
        }
    }
    std::fs::write(&secret_path, secret).expect("write secret key");
    std::fs::write(&public_path, keypair.pk.to_base64()).expect("write public key");

    println!("secret key: {}", secret_path.display());
    println!("public key: {}", public_path.display());
}
