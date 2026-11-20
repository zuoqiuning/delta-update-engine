//! `verify`: re-download a published release, verify signatures and sample
//! patches.

use crate::cli::VerifyArgs;
use crate::pipeline::audit::verify_published;
use crate::pipeline::keys::{normalize_prefix, read_public_keys};
use crate::publisher;

pub fn run(args: &VerifyArgs) -> anyhow::Result<()> {
    verify(args)
}

pub fn verify(args: &VerifyArgs) -> anyhow::Result<()> {
    let publisher = publisher::open(&args.bucket, args.region.as_deref())?;
    let keys = read_public_keys(&args.public_key)?;
    let prefix = normalize_prefix(args.prefix.as_deref())?;
    verify_published(
        publisher.as_ref(),
        &keys,
        &prefix,
        &args.version,
        args.sample,
    )
}
