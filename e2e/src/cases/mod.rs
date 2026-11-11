pub mod crash_loop_rollback;
pub mod cross_volume;
pub mod delta_update;
pub mod fresh_install;
pub mod rollback_release;
pub mod uac_cancelled;

pub struct Case {
    pub name: &'static str,
}

pub const CASES: &[Case] = &[
    Case {
        name: "fresh_install",
    },
    Case {
        name: "delta_update",
    },
    Case {
        name: "crash_loop_rollback",
    },
    Case {
        name: "uac_cancelled",
    },
    Case {
        name: "cross_volume",
    },
    Case {
        name: "rollback_release",
    },
];

pub fn run(name: &str) -> Result<(), String> {
    match name {
        "fresh_install" => fresh_install::run(),
        "delta_update" => delta_update::run(),
        "crash_loop_rollback" => crash_loop_rollback::run(),
        "uac_cancelled" => uac_cancelled::run(),
        "cross_volume" => cross_volume::run(),
        "rollback_release" => rollback_release::run(),
        other => Err(format!("unknown case: {other}")),
    }
}
