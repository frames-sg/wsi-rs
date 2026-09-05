use super::*;
use std::path::Path;

#[test]
fn profile_recipes_include_cpu_commands_for_the_real_worker() {
    let recipes = profile_recipes(
        Path::new("/tmp/fixture.svs"),
        Some("single_tile_l0"),
        "single-tile-profile",
    );

    assert!(recipes.cpu_samply.join(" ").contains("samply record"));
    assert!(recipes.cpu_samply.iter().any(|arg| arg == "--save-only"));
    assert!(recipes.cpu_samply.iter().any(|arg| arg == "env"));
    assert!(recipes
        .cpu_samply
        .iter()
        .any(|arg| arg.starts_with("RAYON_NUM_THREADS=")));
    assert!(recipes
        .cpu_time_profiler
        .join(" ")
        .contains("Time Profiler"));
    if cfg!(target_os = "macos") {
        let launch = recipes
            .cpu_time_profiler
            .iter()
            .position(|arg| arg == "--")
            .expect("xctrace launch separator");
        assert_eq!(recipes.cpu_time_profiler[launch + 1], "/usr/bin/env");
    }
    assert!(recipes.cpu_samply.iter().any(|arg| arg == "--only"));
    assert!(recipes.cpu_samply.iter().any(|arg| arg == "single_tile_l0"));
    assert!(recipes
        .cpu_samply
        .iter()
        .any(|arg| arg.ends_with("wsi-rs-perf")));
    let rayon_threads = recipes
        .cpu_samply
        .iter()
        .find_map(|arg| arg.strip_prefix("RAYON_NUM_THREADS="))
        .expect("profile Rayon budget");
    let workers = recipes
        .cpu_samply
        .windows(2)
        .find(|pair| pair[0] == "--workers")
        .map(|pair| pair[1].as_str())
        .expect("profile worker count");
    assert_eq!(rayon_threads, workers);
}

#[test]
fn profile_cli_rejects_missing_extra_and_non_file_arguments() {
    assert!(profile(vec![]).unwrap_err().contains("usage:"));
    assert!(profile(vec!["a".into(), "b".into(), "c".into()])
        .unwrap_err()
        .contains("usage:"));
    assert!(profile(vec!["definitely-missing-slide".into()])
        .unwrap_err()
        .contains("not a file"));
}

#[test]
fn labels_and_shell_commands_are_sanitized_and_quoted() {
    assert_eq!(
        profile_label(Path::new("case name.svs"), Some("pan/trace")),
        "case-name-pan-trace"
    );
    assert_eq!(
        profile_label(Path::new("case.svs"), None),
        "case-full-suite"
    );
    assert_eq!(sanitize_label("a b+c"), "a-b-c");
    assert_eq!(shell_quote("plain/path"), "plain/path");
    assert_eq!(shell_quote("has space"), "'has space'");
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
    assert_eq!(
        shell_join(&["plain".into(), "has space".into()]),
        "plain 'has space'"
    );
}

#[test]
fn profile_cli_accepts_an_existing_slide_path() {
    let slide = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    profile(vec![slide.display().to_string(), "single_tile_l0".into()]).expect("profile recipe");
}
