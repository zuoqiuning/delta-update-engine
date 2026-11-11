use crate::cli::DoctorArgs;
use crate::pipeline;

pub fn run(args: &DoctorArgs) -> anyhow::Result<()> {
    pipeline::doctor(args)
}
