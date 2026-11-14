//! The `plugins.delta-update` schema lives in the engine, so `delta-update
//! doctor --check-config` and the runtime plugin validate the exact same
//! fields without the CLI depending on Tauri.

pub use delta_update_engine::config::{
    PluginConfig as Config, PLUGIN_CONFIG_VERSION as CONFIG_VERSION,
};
