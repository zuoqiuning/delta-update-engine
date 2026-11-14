use std::path::Path;

/// Static guard for the documented CLI contract.
///
/// Catches the two regressions that matter in practice: a `--json` producer
/// losing the versioned envelope, and the exit-code table disappearing from
/// the contract document.
pub fn run() -> Result<(), String> {
    // (file, minimum envelopes, required markers)
    const JSON_PRODUCERS: &[(&str, usize, &[&str])] = &[
        (
            "crates/delta-update-cli/src/pipeline/mod.rs",
            2,
            &[
                "\"v\": 1",
                "\"tool\": \"delta-update-cli\"",
                "\"releases\": releases",
            ],
        ),
        (
            "crates/delta-update-cli/src/commands/inspect.rs",
            1,
            &["\"v\".into(), 1.into()", "\"tool\".into(), TOOL.into()"],
        ),
        (
            "crates/delta-update-cli/src/pipeline/bundle.rs",
            1,
            &["\"v\": 1", "\"tool\": \"delta-update-cli\""],
        ),
    ];

    let mut errors = Vec::new();

    for (path, minimum, markers) in JSON_PRODUCERS {
        let text = std::fs::read_to_string(path).map_err(|err| format!("read {path}: {err}"))?;
        let envelopes = text.matches(markers[0]).count();
        if envelopes < *minimum {
            errors.push(format!(
                "{path}: expected at least {minimum} JSON envelopes containing {}, found {envelopes}",
                markers[0]
            ));
        }
        for marker in &markers[1..] {
            if !text.contains(marker) {
                errors.push(format!("{path}: missing JSON envelope marker {marker}"));
            }
        }
    }

    match std::fs::read_to_string(Path::new("docs/cli-contract.md")) {
        Ok(text) => {
            for code in ["| 0 |", "| 1 |", "| 2 |", "| 3 |"] {
                if !text.contains(code) {
                    errors.push(format!(
                        "docs/cli-contract.md: missing exit-code row {code}"
                    ));
                }
            }
            if !text.contains("\"v\": 1") || !text.contains("\"tool\": \"delta-update-cli\"") {
                errors.push(
                    "docs/cli-contract.md: JSON envelope example must show v and tool".into(),
                );
            }
        }
        Err(err) => errors.push(format!("read docs/cli-contract.md: {err}")),
    }

    if errors.is_empty() {
        println!("contract: ok");
        Ok(())
    } else {
        for error in &errors {
            eprintln!("{error}");
        }
        Err(format!("{} contract violation(s)", errors.len()))
    }
}
