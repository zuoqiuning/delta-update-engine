use crate::cli::ReportArgs;

/// Summarize release reports written by `release --pipeline`.
pub fn run(args: &ReportArgs) -> anyhow::Result<()> {
    crate::pipeline::report_history(&args.history, args.json)
}
