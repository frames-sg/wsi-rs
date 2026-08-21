use super::*;

fn command_environment(
    command: &Command,
) -> std::collections::BTreeMap<OsString, Option<OsString>> {
    command
        .get_envs()
        .map(|(name, value)| (name.to_os_string(), value.map(OsStr::to_os_string)))
        .collect()
}

#[test]
fn worker_arguments_use_one_real_binary_for_both_engines() {
    let invocation = BenchInvocation {
        worker: PathBuf::from("/tmp/wsi-rs-perf"),
        library: PathBuf::from("/tmp/libopenslide.dylib"),
    };

    let args = worker_args(
        BenchLibrary::OpenSlide,
        &invocation,
        Path::new("/tmp/fixture.svs"),
        2,
        4_096,
        3,
        Some("zoom_trace"),
    );

    assert_eq!(args[0], "--engine");
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--library", "/tmp/libopenslide.dylib"]));
    assert!(args.windows(2).any(|pair| pair == ["--repeat-index", "2"]));
    assert!(args.windows(2).any(|pair| pair == ["--workers", "3"]));
    assert!(args.windows(2).any(|pair| pair == ["--only", "zoom_trace"]));
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--require-version-prefix", "4.0.1"]));
}

#[test]
fn wsi_worker_arguments_omit_openslide_version_and_optional_workload() {
    let invocation = BenchInvocation {
        worker: PathBuf::from("/tmp/wsi-rs-perf"),
        library: PathBuf::from("/tmp/libshim.dylib"),
    };

    let args = worker_args(
        BenchLibrary::WsiRs,
        &invocation,
        Path::new("/tmp/fixture.svs"),
        0,
        DEFAULT_CACHE_BYTES,
        1,
        None,
    );

    assert!(!args.iter().any(|arg| arg == "--only"));
    assert!(!args.iter().any(|arg| arg == "--require-version-prefix"));
    assert_eq!(BenchLibrary::WsiRs.name(), "wsi_rs");
    assert_eq!(BenchLibrary::OpenSlide.name(), "openslide");
    assert_eq!(BenchLibrary::WsiRs.binary(), "wsi-rs-perf");
    assert_eq!(BenchLibrary::WsiRs.required_version_prefix(), None);
    assert_eq!(
        BenchLibrary::OpenSlide.required_version_prefix(),
        Some(PINNED_OPENSLIDE_VERSION)
    );
}

#[test]
fn wsi_rs_library_override_replaces_the_checkout_shim() {
    let target_dir = Path::new("/tmp/current-target");
    let detached = OsStr::new("/tmp/baseline/libwsi_rs_openslide_shim.dylib");

    assert_eq!(
        wsi_rs_library_candidate(target_dir, Some(detached)),
        PathBuf::from(detached)
    );
    assert_eq!(
        wsi_rs_library_candidate(target_dir, None),
        target_dir.join("release").join(shim_library_name())
    );
}

#[test]
fn worker_environment_equalizes_decode_cpu_concurrency() {
    let mut wsi_rs = Command::new("worker");
    let wsi_rs_control = configure_worker_environment(&mut wsi_rs, BenchLibrary::WsiRs, 8);
    let wsi_rs_env = command_environment(&wsi_rs);
    assert_eq!(
        wsi_rs_env.get(OsStr::new("RAYON_NUM_THREADS")),
        Some(&Some(OsString::from("8")))
    );
    assert_eq!(
        wsi_rs_env.get(OsStr::new("WSI_RS_SHIM_JP2K_CPU_THREADS")),
        Some(&Some(OsString::from("1")))
    );
    assert_eq!(wsi_rs_control["rayon_threads_process_wide"], 8);
    assert_eq!(wsi_rs_control["jp2k_threads_per_handle"], 1);
    assert_eq!(wsi_rs_control["active_jp2k_thread_budget"], 8);
    assert_eq!(wsi_rs_control["enforced"], true);

    let mut openslide = Command::new("worker");
    let openslide_control =
        configure_worker_environment(&mut openslide, BenchLibrary::OpenSlide, 8);
    let openslide_env = command_environment(&openslide);
    assert_eq!(
        openslide_env.get(OsStr::new("RAYON_NUM_THREADS")),
        Some(&None)
    );
    assert_eq!(
        openslide_env.get(OsStr::new("WSI_RS_SHIM_JP2K_CPU_THREADS")),
        Some(&None)
    );
    assert_eq!(openslide_control["decoder_threads_per_handle"], 1);
    assert_eq!(openslide_control["active_decode_thread_budget"], 8);
    assert_eq!(openslide_control["enforced"], true);
}

#[test]
fn worker_paths_and_workspace_metadata_are_resolved_fail_closed() {
    let root = workspace_root();
    assert!(root.join("Cargo.toml").is_file());
    assert!(default_public_fixture().is_file());
    assert!(!shim_library_name().is_empty());
    assert!(!result_dir().as_os_str().is_empty());
    assert!(cache_bytes().is_ok_and(|bytes| bytes > 0));
    assert!(target_directory().expect("target directory").is_absolute());
    assert_eq!(
        cargo(),
        std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
    );

    let manifest = root.join("Cargo.toml");
    assert_eq!(
        exact_library_path(&manifest, "fixture").expect("canonical fixture"),
        manifest.canonicalize().expect("canonical manifest")
    );
    assert!(exact_library_path(Path::new("missing-library"), "fixture")
        .unwrap_err()
        .contains("is not a file"));
    assert!(explicit_openslide_library_from(None, None)
        .unwrap_err()
        .contains("requires an exact"));
    assert_eq!(
        explicit_openslide_library_from(Some(manifest.clone().into_os_string()), None)
            .expect("configured OpenSlide path"),
        manifest.canonicalize().expect("canonical manifest")
    );
}

#[test]
fn default_results_are_partitioned_by_host_commit_and_dirty_state() {
    assert_eq!(
        default_result_dir("Mac M4-Pro.local", "772f4f0", Some(true)),
        PathBuf::from("bench/results/mac-m4-pro-local/772f4f0-dirty")
    );
    assert_eq!(
        default_result_dir("wsl-host", "abc123", Some(false)),
        PathBuf::from("bench/results/wsl-host/abc123")
    );
    assert_eq!(
        default_result_dir("host", "unknown", None),
        PathBuf::from("bench/results/host/unknown-state-unknown")
    );
}

#[cfg(unix)]
#[test]
fn worker_process_failure_reports_engine_slide_and_repeat() {
    let invocation = BenchInvocation {
        worker: PathBuf::from("/usr/bin/false"),
        library: workspace_root().join("Cargo.toml"),
    };
    let error = run_bench(
        BenchLibrary::WsiRs,
        &invocation,
        Path::new("fixture.svs"),
        7,
        1_024,
        1,
        None,
    )
    .err()
    .expect("worker must fail");

    assert!(error.contains("wsi-rs-perf"));
    assert!(error.contains("fixture.svs"));
    assert!(error.contains("repeat 7"));
}
