use crate::cli::ReleaseArgs;
use crate::pipeline;

pub fn run(args: &ReleaseArgs) -> anyhow::Result<()> {
    pipeline::release(args)
}
