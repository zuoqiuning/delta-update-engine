const COMMANDS: &[&str] = &[
    "check",
    "prepare",
    "cancel_prepare",
    "status",
    "confirm_healthy",
    "restart_to_apply",
    "export_diagnostics",
    "open_diagnostics_folder",
    "get_update_history",
    "clear_skipped_version",
];

fn main() {
    // `option_env!` in `src/pinned.rs` is only re-read when this changes.
    println!("cargo:rerun-if-env-changed=DELTA_UPDATE_PINNED_PUBLIC_KEYS");
    if std::env::var_os("CARGO_FEATURE_PINNED_KEYS").is_some()
        && std::env::var_os("DELTA_UPDATE_PINNED_PUBLIC_KEYS").is_none()
    {
        panic!(
            "the `pinned-keys` feature requires DELTA_UPDATE_PINNED_PUBLIC_KEYS (comma-separated minisign public keys) at build time"
        );
    }

    tauri_plugin::Builder::new(COMMANDS).build();

    // The mock runtime links TaskDialogIndirect, which only exists in the
    // comctl32 v6 side-by-side assembly. Test targets have no app manifest, so
    // ask the linker to add the dependency there.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!(
            "cargo:rustc-link-arg-tests=/MANIFESTDEPENDENCY:type='win32' \
             name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
             processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
}
