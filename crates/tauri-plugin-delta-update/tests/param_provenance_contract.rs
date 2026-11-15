//! Contract: frontend commands may only receive Tauri handles.
//!
//! Write targets, trusted public keys and application versions must come from
//! the embedded plugin config or the engine. Allowing them as command
//! arguments would let any script inside the webview choose what to replace or
//! whom to trust, which bypasses the whole signature chain.

const COMMANDS: &str = include_str!("../src/commands.rs");

/// Every parameter of every `#[tauri::command]`, taken from the real source so
/// a new command cannot be added without touching this test.
///
/// Errors instead of skipping when the source stops looking like commands: a
/// parser that silently returns nothing would make this contract vacuous.
fn command_parameters(source: &str) -> Result<Vec<String>, String> {
    const MARKER: &str = "#[tauri::command]";
    let mut parameters = Vec::new();
    let mut commands = 0;
    let mut rest = source;
    while let Some(index) = rest.find(MARKER) {
        commands += 1;
        rest = &rest[index + MARKER.len()..];
        let fn_at = rest
            .find("fn ")
            .ok_or_else(|| format!("command {commands} has no `fn`"))?;
        let open = fn_at
            + rest[fn_at..]
                .find('(')
                .ok_or_else(|| format!("command {commands} has no parameter list"))?;
        let close = open
            + rest[open..]
                .find(") ->")
                .ok_or_else(|| format!("command {commands} has no return type"))?;
        for line in rest[open + 1..close].lines().map(str::trim) {
            if !line.is_empty() {
                parameters.push(line.trim_end_matches(',').to_string());
            }
        }
        rest = &rest[close..];
    }
    if commands == 0 {
        return Err(
            "no `#[tauri::command]` found; the source moved or the scanner is stale".into(),
        );
    }
    Ok(parameters)
}

/// The first parameter that is not a Tauri handle, if any.
fn offending_parameter(parameters: &[String]) -> Option<&String> {
    parameters.iter().find(|parameter| {
        !(parameter.starts_with("app: AppHandle<")
            || parameter.starts_with("state: State<")
            // `get_update_history` only takes a clamped result count.
            || parameter.as_str() == "limit: Option<usize>")
    })
}

#[test]
fn commands_only_receive_tauri_handles() {
    let parameters = command_parameters(COMMANDS).expect("scan the real command surface");
    assert!(
        parameters.len() >= 10,
        "expected at least one parameter per command, scanned {}",
        parameters.len()
    );
    if let Some(parameter) = offending_parameter(&parameters) {
        panic!("command parameters must not come from the frontend: {parameter}");
    }
}

#[test]
fn a_command_taking_a_path_is_reported() {
    let fixture = r#"
        #[tauri::command]
        pub async fn write_target(path: PathBuf) -> Result<(), ErrorPayload> {
            Ok(())
        }
    "#;
    let parameters = command_parameters(fixture).unwrap();
    assert_eq!(
        offending_parameter(&parameters).map(String::as_str),
        Some("path: PathBuf")
    );
}

#[test]
fn a_source_without_commands_is_an_error() {
    assert!(command_parameters("fn helper() {}").is_err());
}

#[test]
fn an_unparsable_command_is_an_error() {
    assert!(command_parameters("#[tauri::command]\npub async fn broken").is_err());
}
