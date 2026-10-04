use std::process::Command;

fn xtask() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
}

#[test]
fn help_is_the_default_and_explicit_success_path() {
    for arguments in [Vec::<&str>::new(), vec!["help"], vec!["--help"]] {
        let output = xtask().args(arguments).output().unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("usage: cargo xtask <task>"));
        assert!(stdout.contains("perf-capture-pair"));
    }
}

#[test]
fn unknown_task_returns_a_diagnostic_failure() {
    let output = xtask().arg("not-a-real-task").output().unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("xtask failed: unknown task `not-a-real-task`"));
}

#[cfg(unix)]
mod unix {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::*;

    struct FakeRepository {
        _directory: tempfile::TempDir,
        _tools: tempfile::TempDir,
        _log: tempfile::NamedTempFile,
        root: PathBuf,
        cargo: PathBuf,
        path: OsString,
        log_path: PathBuf,
    }

    impl FakeRepository {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let tools = tempfile::tempdir().unwrap();
            let log = tempfile::NamedTempFile::new().unwrap();
            let root = directory.path().to_path_buf();

            fs::create_dir_all(root.join("api")).unwrap();
            fs::create_dir_all(root.join("fuzz")).unwrap();
            fs::create_dir_all(root.join("scripts")).unwrap();
            fs::create_dir_all(root.join("src")).unwrap();
            fs::create_dir_all(root.join("target")).unwrap();
            fs::write(root.join("Cargo.lock"), "root-lock\n").unwrap();
            fs::write(root.join("fuzz/Cargo.lock"), "fuzz-lock\n").unwrap();
            fs::write(
                root.join("src/lib.rs"),
                "#[cfg(feature = \"fuzzing\")]\nfn fuzz_only() {}\n",
            )
            .unwrap();
            for snapshot in [
                "api/wsi-rs-public-api.txt",
                "api/wsi-rs-public-api-cuda.txt",
                "api/wsi-rs-public-api-metal.txt",
            ] {
                fs::write(root.join(snapshot), "API surface\n").unwrap();
            }
            fs::write(root.join("lcov.info"), complete_lcov()).unwrap();

            let tool_script = "#!/bin/sh\n\
                printf '%s %s\\n' \"$0\" \"$*\" >> \"$XTASK_FAKE_LOG\"\n\
                if [ \"$(basename \"$0\")\" = rustup ]; then printf 'API surface\\n'; fi\n";
            for tool in ["cargo", "cargo-machete", "rustup", "typos"] {
                write_executable(&tools.path().join(tool), tool_script);
            }
            write_executable(
                &root.join("scripts/check-semver.sh"),
                "#!/bin/sh\nprintf 'semver %s\\n' \"$*\" >> \"$XTASK_FAKE_LOG\"\n",
            );

            git(&root, &["init", "--quiet"]);
            git(&root, &["add", "."]);
            git(
                &root,
                &[
                    "-c",
                    "user.name=xtask-test",
                    "-c",
                    "user.email=xtask@example.invalid",
                    "commit",
                    "--quiet",
                    "--no-gpg-sign",
                    "-m",
                    "fixture",
                ],
            );

            let path = std::env::join_paths(std::iter::once(tools.path().to_path_buf()).chain(
                std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
            ))
            .unwrap();
            Self {
                cargo: tools.path().join("cargo"),
                log_path: log.path().to_path_buf(),
                _directory: directory,
                _tools: tools,
                _log: log,
                root,
                path,
            }
        }

        fn run(&self, task: &str, arguments: &[&str]) -> std::process::Output {
            self.command().arg(task).args(arguments).output().unwrap()
        }

        fn command(&self) -> Command {
            let mut command = xtask();
            command
                .current_dir(&self.root)
                .env("CARGO", &self.cargo)
                .env("PATH", &self.path)
                .env("XTASK_FAKE_LOG", &self.log_path)
                .env_remove("WSI_RS_UPDATE_PUBLIC_API")
                .env_remove("WSI_RS_PARITY_ALIASES");
            command
        }
    }

    #[test]
    fn paired_capture_interleaves_optional_baseline_and_keeps_outputs_separate() {
        let fixture = FakeRepository::new();
        let target = fixture.root.join("target");
        let results = fixture.root.join("results");
        let manifest = fixture.root.join("slides.toml");
        fs::write(&manifest, "[[slide]]\nalias='fixture'\nformat='aperio'\npath='tests/fixtures/jp2k/rgb_nomct.j2k'\nmust_decode=['cpu']\n").unwrap();
        fs::create_dir_all(target.join("release")).unwrap();
        for name in ["current", "openslide", "previous"] {
            fs::write(fixture.root.join(name), name).unwrap();
        }
        let worker_log = fixture.root.join("workers.log");
        let script = r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in
        --engine) engine=$2;;
        --library) library=$2;;
        --slide) slide=$2;;
        --repeat-index) repeat=$2;;
        --workers) workers=$2;;
        --compare-library) comparison=$2;;
    esac
    shift 2
done
if [ -n "$comparison" ]; then
    printf '%s\n' '{"reference_version":"4.0.1","workloads":[]}'
    exit 0
fi
printf '%s %s %s\n' "$repeat" "$workers" "${library##*/}" >> "$XTASK_WORKER_LOG"
cat <<EOF
{"schema_version":SCHEMA,"kind":"wsi-rs-perf-worker","engine":"$engine",
"library_path":"$library","library_sha256":"DIGEST","library_version":"test",
"slide_path":"$slide","slide_sha256":"DIGEST","repeat_index":$repeat,
"cache_bytes":268435456,"worker_count":$workers,
"level0_bounds":{"x":0,"y":0,"width":8,"height":8},
"levels":[{"width":8,"height":8,"downsample":1.0}],
"workloads":[{"name":"pan_trace_l0","n":1,"samples_us":[1],
"p50_us":1,"p95_us":1,"p99_us":1,"mean_us":1,"bytes_read":4,
"workers":$workers,"effective_elapsed_us":1,"throughput_bytes_per_second":4000000,
"checksum_sha256":"DIGEST"}]}
EOF
"#
        .replace("SCHEMA", &wsi_rs_perf::WORKER_SCHEMA_VERSION.to_string())
        .replace("DIGEST", &"a".repeat(64));
        write_executable(&target.join("release/wsi-rs-perf"), &script);

        for with_previous in [false, true] {
            fs::write(&worker_log, "").unwrap();
            let label = if with_previous { "triple" } else { "pair" };
            let mut command = fixture.command();
            command
                .args(["perf-capture-pair", label, "fixture"])
                .env("CARGO_TARGET_DIR", &target)
                .env("WSI_RS_PERF_MANIFEST", &manifest)
                .env("WSI_RS_PERF_RESULTS_DIR", &results)
                .env("WSI_RS_PERF_WORKERS", "1,2")
                .env("WSI_RS_PERF_REPEATS", "5")
                .env("WSI_RS_PERF_ONLY", "pan_trace_l0")
                .env("WSI_RS_PERF_CACHE_BYTES", "268435456")
                .env("WSI_RS_BENCH_WSI_RS_LIBRARY", fixture.root.join("current"))
                .env("WSI_RS_OPENSLIDE_LIBRARY", fixture.root.join("openslide"))
                .env("XTASK_WORKER_LOG", &worker_log)
                .env_remove("WSI_RS_BENCH_PREVIOUS_LIBRARY");
            if with_previous {
                command.env(
                    "WSI_RS_BENCH_PREVIOUS_LIBRARY",
                    fixture.root.join("previous"),
                );
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let mut expected = Vec::new();
            for repeat in 0..5 {
                let mut order = vec!["current", "openslide"];
                if with_previous {
                    order.push("previous");
                }
                if repeat % 2 != 0 {
                    order.reverse();
                }
                for workers in [1, 2] {
                    expected.extend(
                        order
                            .iter()
                            .map(|name| format!("{repeat} {workers} {name}")),
                    );
                }
            }
            assert_eq!(
                fs::read_to_string(&worker_log)
                    .unwrap()
                    .lines()
                    .collect::<Vec<_>>(),
                expected
            );
            for (suffix, library) in [
                ("wsi_rs", "current"),
                ("openslide", "openslide"),
                ("previous", "previous"),
            ] {
                let path = results.join(format!("{label}-{suffix}.json"));
                if suffix == "previous" && !with_previous {
                    assert!(!path.exists());
                    continue;
                }
                let capture: serde_json::Value =
                    serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
                let runs = capture["runs"].as_array().unwrap();
                assert_eq!(runs.len(), 10);
                assert!(
                    runs.iter()
                        .all(|run| Path::new(run["library_path"].as_str().unwrap())
                            .ends_with(library))
                );
                assert_eq!(
                    runs.iter()
                        .filter(|run| run.get("pixel_comparison").is_some())
                        .count(),
                    usize::from(suffix == "wsi_rs")
                );
            }
        }
    }

    #[test]
    fn engineering_commands_execute_their_complete_orchestration_contracts() {
        let fixture = FakeRepository::new();
        for (task, arguments) in [
            ("ci", vec![]),
            ("test", vec![]),
            ("parity-corpus-test", vec![]),
            ("release-test", vec![]),
            ("typos", vec![]),
            ("coverage", vec![]),
        ] {
            let output = fixture.run(task, &arguments);
            assert!(
                output.status.success(),
                "{task} failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fs::write(
            fixture.root.join("src/lib.rs"),
            "#[cfg(feature = \"fuzzing\")]\nfn fuzz_only() {}\nmod candidate;\npub use candidate::is_candidate;\n",
        )
        .unwrap();
        let output = fixture.run(
            "coverage-changed",
            &["--base", "HEAD", "--lcov", "lcov.info"],
        );
        assert!(
            output.status.success(),
            "coverage-changed failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let output = fixture.run("rc-preflight", &[]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("WSI_RS_RC_OPENSLIDE_CAPTURE"));
        let log = fs::read_to_string(&fixture.log_path).unwrap();
        for expected in [
            "public-api -p wsi-rs",
            "cargo-machete",
            "fuzz check open_zvi_bytes",
            "fuzz run --sanitizer address open_vsi_bundle_bytes",
            "hack check --locked --workspace",
            "nextest run --locked",
            "package --locked",
            "publish --dry-run --locked",
            "test --locked --test openslide_parity",
            "test --locked --lib --tests --release",
            "typos",
            "llvm-cov --locked --workspace",
        ] {
            assert!(log.contains(expected), "missing `{expected}` in:\n{log}");
        }
    }

    fn write_executable(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn git(root: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git {} failed", arguments.join(" "));
    }

    fn complete_lcov() -> String {
        let roots = [
            "src/core.rs",
            "src/decode.rs",
            "src/formats/dicom.rs",
            "src/formats/hamamatsu_vms.rs",
            "src/formats/mirax.rs",
            "src/formats/olympus_vsi.rs",
            "src/formats/raw_jp2k.rs",
            "src/formats/svcache.rs",
            "src/formats/tiff_family.rs",
            "src/formats/tiff_family/layout/generic.rs",
            "src/formats/tiff_family/layout/aperio.rs",
            "src/formats/tiff_family/layout/argos.rs",
            "src/formats/tiff_family/layout/huron.rs",
            "src/formats/tiff_family/layout/ndpi.rs",
            "src/formats/tiff_family/layout/leica.rs",
            "src/formats/tiff_family/layout/philips.rs",
            "src/formats/tiff_family/layout/trestle.rs",
            "src/formats/tiff_family/layout/ventana.rs",
            "src/formats/zeiss.rs",
            "src/formats/zeiss_zvi.rs",
            "wsi-rs-openslide-shim/src/lib.rs",
            "xtask/src/lib.rs",
            "perf-runner/src/lib.rs",
        ];
        roots
            .iter()
            .enumerate()
            .map(|(index, path)| {
                format!(
                    "SF:{path}\nFN:1,function_{index}\nFNDA:1,function_{index}\nDA:1,1\nend_of_record\n"
                )
            })
            .collect()
    }
}
