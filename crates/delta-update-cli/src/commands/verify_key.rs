use crate::cli::VerifyKeyArgs;
use crate::pipeline;

pub fn run(args: &VerifyKeyArgs) -> anyhow::Result<()> {
    pipeline::verify_key(args)
}
