//! Contract: frontend commands may only receive Tauri handles.
//!
//! Write targets, trusted public keys and application versions must come from
//! the embedded plugin config or the engine. Allowing them as command
//! arguments would let any script inside the webview choose what to replace or
//! whom to trust, which bypasses the whole signature chain.

const COMMANDS: &str = include_str!("../src/commands.rs");

/// Every parameter of every `#[tauri::command]`, taken from the real source so
/// a new command cannot be added without touching this test.
fn command_parameters(source: &str) -> Vec<String> {
    const MARKER: &str = "#[tauri::command]";
    let mut parameters = Vec::new();
    let mut rest = source;
    while let Some(index) = rest.find(MARKER) {
        rest = &rest[index + MARKER.len()..];
        let fn_at = rest.find("fn ").expect("command without fn");
        let open = fn_at + rest[fn_at..].find('(').expect("command without params");
        let close = open
            + rest[open..]
                .find(") ->")
                .expect("command without return type");
        for line in rest[open + 1..close].lines().map(str::trim) {
            if !line.is_empty() {
                parameters.push(line.trim_end_matches(',').to_string());
            }
        }
        rest = &rest[close..];
    }
    parameters
}

#[test]
fn commands_only_receive_tauri_handles() {
    let parameters = command_parameters(COMMANDS);
    assert!(
        parameters.len() >= 10,
        "expected at least one parameter per command, scanned {}",
        parameters.len()
    );
    for parameter in &parameters {
        let allowed = parameter.starts_with("app: AppHandle<")
            || parameter.starts_with("state: State<")
            // `get_update_history` only takes a clamped result count.
            || parameter == "limit: Option<usize>";
        assert!(
            allowed,
            "command parameters must not come from the frontend: {parameter}"
        );
    }
}
