//! `verify-key`: check that a minisign secret key matches a public key.

use anyhow::{bail, Context};

use crate::cli::VerifyKeyArgs;
use crate::output;
use crate::pipeline::sign;

pub fn run(args: &VerifyKeyArgs) -> anyhow::Result<()> {
    verify_key(args)
}

pub fn verify_key(args: &VerifyKeyArgs) -> anyhow::Result<()> {
    let secret_key = sign::load_secret_key(&args.key)?;
    let public_key = minisign::PublicKey::from_secret_key(&secret_key)
        .context("derive public key from secret key")?
        .to_base64();
    let expected: Vec<&str> = args
        .public
        .split(',')
        .flat_map(str::lines)
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.starts_with("untrusted comment:"))
        .collect();
    if expected.is_empty() {
        bail!("--public must contain at least one public key");
    }
    let matches = expected.contains(&public_key.as_str());
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "v": 1,
                "tool": "delta-update-cli",
                "matches": matches,
                "public_key": public_key,
            }))?
        );
    }
    if matches {
        if !args.json {
            output::info(&format!("key pair matches: {public_key}"));
        }
        Ok(())
    } else {
        bail!("secret key does not match any provided public key")
    }
}
