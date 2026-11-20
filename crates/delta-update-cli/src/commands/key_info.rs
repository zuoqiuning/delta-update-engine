//! `key-info`: derive the base64 public key and key id from a secret key.

use anyhow::Context;

use crate::cli::KeyInfoArgs;
use crate::output;
use crate::pipeline::sign;

pub fn run(args: &KeyInfoArgs) -> anyhow::Result<()> {
    key_info(args)
}

pub fn key_info(args: &KeyInfoArgs) -> anyhow::Result<()> {
    let secret_key = sign::load_secret_key(&args.key)?;
    let public_key = minisign::PublicKey::from_secret_key(&secret_key)
        .context("derive public key from secret key")?;
    let key_id = public_key
        .keynum()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "v": 1,
                "tool": "delta-update-cli",
                "public_key": public_key.to_base64(),
                "key_id": key_id,
            }))?
        );
    } else {
        output::info(&format!("public key: {}", public_key.to_base64()));
        output::info(&format!("key id: {key_id}"));
    }
    Ok(())
}
