use crate::cli::KeyInfoArgs;
use crate::pipeline;

pub fn run(args: &KeyInfoArgs) -> anyhow::Result<()> {
    pipeline::key_info(args)
}
