const COMMANDS: &[&str] = &[
    "check",
    "prepare",
    "cancel_prepare",
    "status",
    "confirm_healthy",
    "restart_to_apply",
];

fn main() {
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
