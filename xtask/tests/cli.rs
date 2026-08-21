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
            fs::create_dir_all(root.join("target")).unwrap();
            fs::write(root.join("Cargo.lock"), "root-lock\n").unwrap();
            fs::write(root.join("fuzz/Cargo.lock"), "fuzz-lock\n").unwrap();
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
            xtask()
                .arg(task)
                .args(arguments)
                .current_dir(&self.root)
                .env("CARGO", &self.cargo)
                .env("PATH", &self.path)
                .env("XTASK_FAKE_LOG", &self.log_path)
                .env_remove("WSI_RS_UPDATE_PUBLIC_API")
                .env_remove("WSI_RS_PARITY_ALIASES")
                .output()
                .unwrap()
        }
    }

    #[test]
    fn engineering_commands_execute_their_complete_orchestration_contracts() {
        let fixture = FakeRepository::new();
        for (task, arguments) in [
            ("rc-preflight", Vec::<&str>::new()),
            ("ci", vec![]),
            ("test", vec![]),
            ("parity-corpus-test", vec![]),
            ("release-test", vec![]),
            ("typos", vec![]),
            ("coverage", vec![]),
            (
                "coverage-changed",
                vec!["--base", "HEAD", "--lcov", "lcov.info"],
            ),
        ] {
            let output = fixture.run(task, &arguments);
            assert!(
                output.status.success(),
                "{task} failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let log = fs::read_to_string(&fixture.log_path).unwrap();
        for expected in [
            "public-api -p wsi-rs",
            "cargo-machete",
            "fuzz check open_zvi_bytes",
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
