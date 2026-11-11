use crate::cli::VerifyArgs;
use crate::pipeline;

pub fn run(args: &VerifyArgs) -> anyhow::Result<()> {
    pipeline::verify(args)
}
