use super::*;

fn required_args() -> Vec<String> {
    [
        "--engine",
        "wsi_rs",
        "--library",
        "/tmp/libshim.dylib",
        "--slide",
        "/tmp/fixture.svs",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[test]
fn parses_required_and_optional_worker_arguments() {
    let mut args = required_args();
    args.extend(
        [
            "--repeat-index",
            "2",
            "--cache-bytes",
            "4096",
            "--only",
            "zoom_trace",
            "--workers",
            "2",
            "--require-version-prefix",
            "4.0.1",
            "--compare-library",
            "/tmp/libopenslide.dylib",
        ]
        .into_iter()
        .map(str::to_string),
    );

    let config = WorkerConfig::parse(args).expect("valid arguments");

    assert_eq!(config.engine, Engine::WsiRs);
    assert_eq!(config.repeat_index, 2);
    assert_eq!(config.cache_bytes, 4_096);
    assert_eq!(config.workers, 2);
    assert_eq!(config.only.as_deref(), Some("zoom_trace"));
    assert_eq!(config.required_version_prefix.as_deref(), Some("4.0.1"));
    assert_eq!(
        config.comparison_library.as_deref(),
        Some(std::path::Path::new("/tmp/libopenslide.dylib"))
    );
}

#[test]
fn uses_reproducible_defaults() {
    let config = WorkerConfig::parse(required_args()).expect("valid arguments");

    assert_eq!(config.repeat_index, 0);
    assert_eq!(config.cache_bytes, DEFAULT_CACHE_BYTES);
    assert_eq!(DEFAULT_CACHE_BYTES, 256 * 1024 * 1024);
    assert_eq!(config.workers, 1);
    assert_eq!(config.only, None);
    assert_eq!(config.required_version_prefix, None);
}

#[test]
fn serialized_arguments_parse_back_to_the_same_configuration() {
    let full = WorkerConfig {
        engine: Engine::OpenSlide,
        library_path: PathBuf::from("/tmp/libopenslide.dylib"),
        slide_path: PathBuf::from("/tmp/slides/fixture with spaces.svs"),
        repeat_index: 3,
        cache_bytes: 4_096,
        workers: 2,
        only: Some("zoom_trace".into()),
        required_version_prefix: Some(PINNED_OPENSLIDE_VERSION.into()),
        comparison_library: Some(PathBuf::from("/tmp/reference/libopenslide.dylib")),
    };
    let minimal = WorkerConfig {
        engine: Engine::WsiRs,
        only: None,
        required_version_prefix: None,
        comparison_library: None,
        ..full.clone()
    };

    for config in [full, minimal] {
        let args = config.to_args();
        assert_eq!(WorkerConfig::parse(args.clone()), Ok(config), "{args:?}");
    }
}

#[test]
fn serialized_arguments_omit_absent_optional_flags() {
    let args = WorkerConfig::parse(required_args())
        .expect("valid arguments")
        .to_args();

    for flag in ["--only", "--require-version-prefix", "--compare-library"] {
        assert!(!args.iter().any(|arg| arg == flag), "{flag} in {args:?}");
    }
}

#[test]
fn rejects_unknown_duplicate_missing_and_invalid_arguments() {
    assert!(WorkerConfig::parse(["--wat".into()]).is_err());
    assert!(WorkerConfig::parse([
        "--engine".into(),
        "invalid".into(),
        "--library".into(),
        "x".into(),
        "--slide".into(),
        "y".into(),
    ])
    .is_err());

    let mut duplicate = required_args();
    duplicate.extend(["--engine".into(), "openslide".into()]);
    assert!(WorkerConfig::parse(duplicate).is_err());

    let mut zero_cache = required_args();
    zero_cache.extend(["--cache-bytes".into(), "0".into()]);
    assert!(WorkerConfig::parse(zero_cache).is_err());

    let mut zero_workers = required_args();
    zero_workers.extend(["--workers".into(), "0".into()]);
    assert!(WorkerConfig::parse(zero_workers).is_err());

    assert!(WorkerConfig::parse(Vec::<String>::new()).is_err());
}

#[test]
fn rejects_every_malformed_value_and_missing_required_field() {
    for invalid in [
        vec!["--engine"],
        vec!["--engine", "wsi_rs"],
        vec!["--engine", "wsi_rs", "--library", "lib"],
    ] {
        assert!(WorkerConfig::parse(invalid.into_iter().map(str::to_string)).is_err());
    }

    for (flag, value) in [
        ("--repeat-index", "nope"),
        ("--cache-bytes", "nope"),
        ("--workers", "nope"),
        ("--only", ""),
        ("--require-version-prefix", ""),
    ] {
        let mut args = required_args();
        args.extend([flag.to_string(), value.to_string()]);
        assert!(WorkerConfig::parse(args).is_err(), "{flag}={value:?}");
    }
}

#[test]
fn engine_names_match_worker_cli_values() {
    assert_eq!(Engine::WsiRs.name(), "wsi_rs");
    assert_eq!(Engine::OpenSlide.name(), "openslide");
}
