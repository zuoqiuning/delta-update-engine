use crate::cli::RollbackArgs;

/// Point `manifests/latest.json` at an already published version.
pub fn run(args: &RollbackArgs) -> anyhow::Result<()> {
    crate::pipeline::rollback(args)
}
