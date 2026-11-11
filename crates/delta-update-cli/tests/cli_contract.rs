use clap::Parser;
use delta_update_cli::cli::{Cli, Command};
use delta_update_cli::ignore::DeltaIgnore;

#[test]
fn release_has_safe_defaults() {
    let cli = Cli::try_parse_from([
        "delta-update",
        "release",
        "--to",
        "dist",
        "--version",
        "1.2.3",
        "--bucket",
        "cos://example",
        "--key",
        "release.key",
    ])
    .unwrap();

    let Command::Release(args) = cli.command else {
        panic!("expected release");
    };
    assert_eq!(args.version, "1.2.3");
    assert_eq!(args.channel, "stable");
    assert!((args.delta_max_ratio - 0.60).abs() < f64::EPSILON);
    assert!(!args.dry_run);
    assert!(!args.require_delta);
}

#[test]
fn doctor_accepts_multiple_endpoints() {
    let cli = Cli::try_parse_from([
        "delta-update",
        "doctor",
        "--endpoint",
        "https://a/latest.json",
        "--endpoint",
        "https://b/latest.json",
    ])
    .unwrap();

    let Command::Doctor(args) = cli.command else {
        panic!("expected doctor");
    };
    assert_eq!(args.endpoint.len(), 2);
}

#[test]
fn default_ignore_excludes_debug_symbols() {
    let ignore = DeltaIgnore::default();
    assert!(ignore.is_ignored("App.pdb"));
    assert!(ignore.is_ignored("logs/app.log"));
    assert!(!ignore.is_ignored("App.exe"));
}
