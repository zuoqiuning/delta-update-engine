use tauri_plugin_delta_update::BuilderExt;

fn main() {
    // `run_with_delta_update` handles the replacement/recovery worker before
    // any GUI is created, so worker mode cannot be missed by ordering.
    tauri::Builder::default()
        .plugin(tauri_plugin_delta_update::init())
        .run_with_delta_update(tauri::generate_context!())
        .expect("error while running minimal delta app");
}
