use crate::cli::MigrateArgs;
use crate::pipeline;

pub fn run(args: &MigrateArgs) -> anyhow::Result<()> {
    pipeline::migrate(args)
}
