use delta_update_engine::StatePayload;
use tauri::{AppHandle, Emitter, Runtime};

pub const STATE_EVENT: &str = "delta-update://state";

pub fn emit_state<R: Runtime>(app: &AppHandle<R>, payload: StatePayload) {
    if let Err(error) = app.emit(STATE_EVENT, payload) {
        tracing::warn!(%error, "failed to emit update state");
    }
}
