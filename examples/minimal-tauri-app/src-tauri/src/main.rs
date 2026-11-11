fn main() {
    // Replacement mode: no Tauri, no window, just the journal.
    if let Some(code) = tauri_plugin_delta_update::run_applier_from_args() {
        std::process::exit(code);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_delta_update::init())
        .run(tauri::generate_context!())
        .expect("error while running minimal delta app");
}
