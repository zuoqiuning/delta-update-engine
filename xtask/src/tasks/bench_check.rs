//! Performance regression gate.
//!
//! CI saves criterion baselines on `main` (`cargo bench -- --save-baseline main`)
//! and restores `target/criterion` on PRs. Running `cargo bench -- --baseline
//! main` then writes `change/estimates.json` next to each benchmark; this task
//! reads those files instead of scraping console output.

use std::path::{Path, PathBuf};

const DEFAULT_THRESHOLD: f64 = 0.10;

pub fn run() -> Result<(), String> {
    let threshold = parse_threshold()?;
    let root = Path::new("target/criterion");
    if !root.is_dir() {
        println!(
            "bench-check: {} not found; run `cargo bench -- --baseline main` first",
            root.display()
        );
        return Ok(());
    }

    let mut current = Vec::new();
    let mut compared = Vec::new();
    visit(root, &mut current, &mut compared)?;

    let mut regressions = Vec::new();
    for bench in &compared {
        let estimates = bench.join("change/estimates.json");
        let Ok(text) = std::fs::read_to_string(&estimates) else {
            continue;
        };
        let Some((point, upper)) = mean_change(&text) else {
            continue;
        };
        if point > threshold && upper > threshold {
            regressions.push((display_bench(bench), point, upper));
        }
    }

    let missing: Vec<PathBuf> = current
        .iter()
        .filter(|bench| !bench.join("change/estimates.json").exists())
        .map(|bench| bench.to_path_buf())
        .collect();
    for bench in missing {
        println!(
            "bench-check: warning: {} has no baseline comparison (renamed or new benchmark?)",
            display_bench(&bench)
        );
    }

    if regressions.is_empty() {
        println!(
            "bench-check: ok ({} compared, threshold {:.0}%)",
            compared.len(),
            threshold * 100.0
        );
        return Ok(());
    }
    for (bench, point, upper) in &regressions {
        eprintln!(
            "bench-check: {bench} regressed {:+.1}% (upper bound {:+.1}%)",
            point * 100.0,
            upper * 100.0
        );
    }
    Err(format!("{} benchmark regression(s)", regressions.len()))
}

fn parse_threshold() -> Result<f64, String> {
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        if arg == "--threshold" {
            let value = args
                .next()
                .ok_or_else(|| "--threshold requires a value".to_string())?;
            let parsed: f64 = value
                .parse()
                .map_err(|_| format!("invalid --threshold: {value}"))?;
            if !(0.0..1.0).contains(&parsed) {
                return Err("--threshold must be between 0 and 1".into());
            }
            return Ok(parsed);
        }
    }
    Ok(DEFAULT_THRESHOLD)
}

/// Collect benchmark directories: `new/` marks what this run measured, and a
/// sibling `change/estimates.json` means criterion compared it to a baseline.
fn visit(
    dir: &Path,
    current: &mut Vec<PathBuf>,
    compared: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|err| format!("read {}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        if !entry.file_type().map_err(|err| err.to_string())?.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        match name.as_ref() {
            "new" => {
                if let Some(bench) = path.parent() {
                    current.push(bench.to_path_buf());
                }
            }
            "change" => {
                if path.join("estimates.json").exists() {
                    if let Some(bench) = path.parent() {
                        compared.push(bench.to_path_buf());
                    }
                }
            }
            _ => visit(&path, current, compared)?,
        }
    }
    Ok(())
}

fn display_bench(bench: &Path) -> String {
    bench
        .strip_prefix("target/criterion")
        .unwrap_or(bench)
        .display()
        .to_string()
}

/// Read `mean.point_estimate` and `mean.confidence_interval.upper_bound` from a
/// criterion `estimates.json` without pulling a JSON dependency into xtask.
fn mean_change(text: &str) -> Option<(f64, f64)> {
    let start = text.find("\"mean\"")?;
    let open = start + text[start..].find('{')?;
    let object = json_object(&text[open..])?;
    let point = number_field(object, "point_estimate")?;
    let upper = number_field(object, "upper_bound").unwrap_or(point);
    Some((point, upper))
}

fn json_object(text: &str) -> Option<&str> {
    let mut depth = 0_usize;
    for (index, character) in text.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

fn number_field(object: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\":");
    let start = object.find(&needle)? + needle.len();
    let rest = object[start..].trim_start();
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest[..end].trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_mean_estimate_and_upper_bound() {
        let text = r#"{"mean":{"confidence_interval":{"confidence_level":0.95,"lower_bound":0.1,"upper_bound":0.2},"point_estimate":0.15,"standard_error":0.01}}"#;
        assert_eq!(mean_change(text), Some((0.15, 0.2)));
    }

    #[test]
    fn missing_mean_is_ignored() {
        assert_eq!(mean_change(r#"{"median":{"point_estimate":1.0}}"#), None);
    }
}
