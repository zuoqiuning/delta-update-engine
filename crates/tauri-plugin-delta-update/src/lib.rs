//! Tauri 2 adapter for the delta update engine.

// One unsafe call site is allowed in `applier` for ShellExecuteW("runas");
// everything else stays safe Rust.
#![deny(unsafe_code)]

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{RunEvent, Runtime};

pub mod applier;
pub mod commands;
pub mod config;
pub mod desktop;
pub mod events;
pub mod lifecycle;
pub mod state;

pub use config::Config;
pub use delta_update_engine::apply::{run_applier, run_applier_from_args};
pub use events::STATE_EVENT;

pub fn init<R: Runtime>() -> TauriPlugin<R, Config> {
    Builder::<R, Config>::new("delta-update")
        .invoke_handler(tauri::generate_handler![
            commands::check,
            commands::prepare,
            commands::cancel_prepare,
            commands::status,
            commands::confirm_healthy,
            commands::restart_to_apply,
        ])
        .setup(|app, api| {
            api.config().validate()?;
            lifecycle::setup(app, api.config())?;
            Ok(())
        })
        .on_window_ready(|window| lifecycle::on_window_ready(&window))
        .on_event(|app, event| {
            if matches!(event, RunEvent::Exit) {
                lifecycle::on_exit(app);
            }
        })
        .build()
}
