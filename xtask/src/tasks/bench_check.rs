//! Performance regression gate.
//!
//! CI saves criterion baselines on `main` (`cargo bench -- --save-baseline main`)
//! and restores `target/criterion` on PRs. Running `cargo bench -- --baseline
//! main` then writes `change/estimates.json` next to each benchmark; this task
//! reads those files instead of scraping console output.
//!
//! Candidate runs are compared only against a baseline from the same CPU model.
//! `windows-latest` rotates hardware, and a different runner slows every
//! benchmark at once — a uniform "+40%" means "different machine", not a
//! regression.

use std::path::{Path, PathBuf};

const DEFAULT_THRESHOLD: f64 = 0.10;
/// CPU-bound benchmarks are less noisy than I/O-heavy ones, so a 5% regression
/// is meaningful there while the same number would fire on unrelated disk load
/// for everything else.
const DEFAULT_CPU_THRESHOLD: f64 = 0.05;
const CPU_BOUND_PREFIXES: &[&str] = &["scan_tree", "patch_header", "hash_tree"];
/// Written next to the baseline so a later run can tell whether it is
/// comparable. Missing in baselines saved before this file existed.
const RUNNER_FILE: &str = "runner.json";

struct Thresholds {
    io: f64,
    cpu: f64,
}

impl Thresholds {
    fn for_bench(&self, name: &str) -> f64 {
        if CPU_BOUND_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            self.cpu
        } else {
            self.io
        }
    }
}

pub fn run() -> Result<(), String> {
    let thresholds = parse_thresholds()?;
    let root = Path::new("target/criterion");
    if !root.is_dir() {
        println!(
            "bench-check: {} not found; run `cargo bench -- --baseline main` first",
            root.display()
        );
        return Ok(());
    }
    if let Some(reason) = not_comparable(root) {
        println!("bench-check: {reason}; skipping comparison");
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
        let name = display_bench(bench);
        let threshold = thresholds.for_bench(&name);
        if point > threshold && upper > threshold {
            regressions.push((name, point, upper, threshold));
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
            "bench-check: ok ({} compared, thresholds {:.0}% I/O / {:.0}% CPU)",
            compared.len(),
            thresholds.io * 100.0,
            thresholds.cpu * 100.0
        );
        return Ok(());
    }
    for (bench, point, upper, threshold) in &regressions {
        eprintln!(
            "bench-check: {bench} regressed {:+.1}% (upper bound {:+.1}%, threshold {:.0}%)",
            point * 100.0,
            upper * 100.0,
            threshold * 100.0
        );
    }
    Err(format!("{} benchmark regression(s)", regressions.len()))
}

/// Why a comparison would be meaningless, or `None` when it is safe to compare.
///
/// Both sides must come from the same CPU model: shared runners rotate
/// hardware, and a swap slows every benchmark at once.
fn not_comparable(root: &Path) -> Option<String> {
    let current = match current_cpu() {
        Some(cpu) => cpu,
        None => return Some("cannot read the local CPU model".into()),
    };
    let recorded = std::fs::read_to_string(root.join(RUNNER_FILE))
        .ok()
        .and_then(|text| string_field(&text, "cpu"));
    let Some(recorded) = recorded else {
        return Some(format!(
            "baseline has no {RUNNER_FILE} (saved before the runner gate existed)"
        ));
    };
    if recorded.trim() == current {
        return None;
    }
    Some(format!(
        "baseline ran on `{recorded}`, this run is on `{current}`"
    ))
}

fn current_cpu() -> Option<String> {
    let mut system = sysinfo::System::new();
    system.refresh_cpu_all();
    system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim().to_string())
}

fn parse_thresholds() -> Result<Thresholds, String> {
    let mut thresholds = Thresholds {
        io: DEFAULT_THRESHOLD,
        cpu: DEFAULT_CPU_THRESHOLD,
    };
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        let target = match arg.as_str() {
            "--threshold" => &mut thresholds.io,
            "--cpu-threshold" => &mut thresholds.cpu,
            _ => continue,
        };
        let value = args
            .next()
            .ok_or_else(|| format!("{arg} requires a value"))?;
        let parsed: f64 = value
            .parse()
            .map_err(|_| format!("invalid {arg}: {value}"))?;
        if !(0.0..1.0).contains(&parsed) {
            return Err(format!("{arg} must be between 0 and 1"));
        }
        *target = parsed;
    }
    Ok(thresholds)
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

fn string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
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

    #[test]
    fn reads_the_runner_cpu_model() {
        let text = r#"{"cpu":"AMD EPYC 7763 64-Core Processor","cores":4}"#;
        assert_eq!(
            string_field(text, "cpu").as_deref(),
            Some("AMD EPYC 7763 64-Core Processor")
        );
        assert_eq!(string_field(r#"{"cpu":null}"#, "cpu"), None);
    }

    #[test]
    fn cpu_bound_benchmarks_use_the_tighter_threshold() {
        let thresholds = Thresholds {
            io: 0.10,
            cpu: 0.05,
        };
        assert_eq!(thresholds.for_bench("scan_tree/cold"), 0.05);
        assert_eq!(thresholds.for_bench("patch_header/64KiB"), 0.05);
        assert_eq!(thresholds.for_bench("hash_tree"), 0.05);
        assert_eq!(thresholds.for_bench("journal_verify/1000"), 0.10);
        assert_eq!(thresholds.for_bench("apply_commit"), 0.10);
    }
}
