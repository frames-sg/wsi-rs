use super::support::*;
use std::fs;
use std::io::Write;
use std::process::Command;
use wsi_rs::FormatRegistry;

#[test]
fn session_audit_reports_are_not_tracked() {
    let output = Command::new("git")
        .current_dir(crate_root())
        .args(["ls-files", "--", "docs/audit", "docs/audits"])
        .output()
        .expect("list tracked audit reports");
    assert!(output.status.success(), "git ls-files must succeed");
    assert!(
        output.stdout.is_empty(),
        "session audit reports belong in ignored .local-docs/, not Git:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn publish_workflow_runs_the_supply_chain_gate() {
    let workflow = fs::read_to_string(crate_root().join(".github/workflows/publish.yml"))
        .expect("read publish workflow");
    assert!(
        workflow.contains("cargo xtask deps"),
        "publishing must depend on the repository supply-chain checks"
    );
}

#[test]
fn public_manifest_does_not_depend_on_local_path_patches() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read manifest");
    let manifest = manifest
        .parse::<toml::Value>()
        .expect("Cargo.toml must parse as TOML");

    let Some(patches) = manifest.get("patch").and_then(toml::Value::as_table) else {
        return;
    };
    let Some(crates_io) = patches.get("crates-io").and_then(toml::Value::as_table) else {
        return;
    };

    let local_path_patches = crates_io
        .iter()
        .filter_map(|(name, value)| {
            value
                .as_table()
                .and_then(|table| table.get("path"))
                .and_then(toml::Value::as_str)
                .map(|path| format!("{name} -> {path}"))
        })
        .collect::<Vec<_>>();

    assert!(
        local_path_patches.is_empty(),
        "public manifest must verify against registry dependencies, not local path patches:\n{}",
        local_path_patches.join("\n")
    );
}

#[test]
fn tracked_text_files_do_not_contain_local_user_paths() {
    let mut offenders = Vec::new();
    let local_user_path = ["/Users", "user", ""].join("/");
    for path in tracked_text_files(crate_root()) {
        let source = fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!("read {}: {err}", path.display());
        });
        if source.contains(&local_user_path) {
            offenders.push(relative_path(&path));
        }
    }

    assert!(
        offenders.is_empty(),
        "tracked text files must not contain local /Users/user paths:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn tracked_text_files_do_not_reference_agent_private_artifacts() {
    let private_docs_name = ["super", "powers"].concat();
    let private_dir = ["docs", private_docs_name.as_str()].join("/");
    let migration_doc = ["MIGRATION", ".md"].concat();
    let migration_doc_lower = migration_doc.to_ascii_lowercase();
    let mut offenders = Vec::new();

    for path in tracked_text_files(crate_root()) {
        let relative = relative_path(&path);
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if relative.starts_with(&private_dir) || file_name == migration_doc_lower {
            offenders.push(relative);
        }
    }

    assert!(
        offenders.is_empty(),
        "tracked text files must not include agent-private planning docs or migration scratch files:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn public_docs_use_production_library_language() {
    let public_docs = [
        "README.md",
        "CHANGELOG.md",
        "SECURITY.md",
        "wsi-rs-openslide-shim/README.md",
        "tests/fixtures/jp2k/README.md",
        "src/lib.rs",
    ];
    let forbidden = [
        ["L", "L", "M"].concat(),
        ["A", "I", "-assisted"].concat(),
        ["agent", " or engineer"].concat(),
    ];
    let mut offenders = Vec::new();

    for relative in public_docs {
        let text = fs::read_to_string(crate_root().join(relative))
            .unwrap_or_else(|err| panic!("read {relative}: {err}"));
        for term in &forbidden {
            if text.contains(term) {
                offenders.push(format!("{relative}: {term}"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "public docs should read like production library documentation, not assistant-facing prompt material:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn referenced_parity_corpus_fetch_script_exists() {
    let script = crate_root().join("scripts/parity-corpus-fetch.sh");
    assert!(
        script.is_file(),
        "tests reference scripts/parity-corpus-fetch.sh, so the script must exist"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&script)
            .expect("stat fetch script")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "scripts/parity-corpus-fetch.sh must be executable"
        );
    }
}

#[test]
fn corpus_fetch_script_names_cached_slides_like_the_rust_lookup() {
    let script = fs::read_to_string(crate_root().join("scripts/parity-corpus-fetch.sh"))
        .expect("read fetch script");
    let table = script
        .split_once("FORMAT_EXTENSIONS = {")
        .and_then(|(_, rest)| rest.split_once('}'))
        .map(|(table, _)| table)
        .expect("fetch script defines FORMAT_EXTENSIONS");
    let python = table
        .lines()
        .filter_map(|line| {
            let (format, extension) = line.trim().trim_end_matches(',').split_once(':')?;
            Some((
                format.trim().trim_matches('"').to_string(),
                extension.trim().trim_matches('"').to_string(),
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert!(!python.is_empty(), "FORMAT_EXTENSIONS parsed no entries");

    let manifest = wsi_rs_test_support::corpus::load_manifest(
        &crate_root().join("tests/fixtures/parity_corpus.public.toml"),
    )
    .expect("load public manifest");
    let formats = python
        .keys()
        .cloned()
        .chain(manifest.slides.into_iter().map(|slide| slide.format))
        .collect::<std::collections::BTreeSet<_>>();
    for format in formats {
        assert_eq!(
            python.get(&format).map(String::as_str),
            wsi_rs_test_support::corpus::format_default_extension(&format),
            "fetch script and format_default_extension disagree on {format}"
        );
    }
}

#[test]
fn corpus_ci_reuses_verified_parent_cache_and_serializes_coverage() {
    let workflow = fs::read_to_string(crate_root().join(".github/workflows/ci.yml"))
        .expect("read CI workflow")
        .replace("\r\n", "\n");

    assert!(
        workflow.contains("  coverage:\n    needs: parity-corpus\n"),
        "coverage must wait for parity-corpus to populate the shared corpus cache"
    );
    assert_eq!(
        workflow
            .matches("restore-keys: |\n            wsi-rs-parity-corpus-\n")
            .count(),
        2,
        "both corpus cache consumers must restore a verified parent-layer cache"
    );
}

fn zeiss_magic_fixture(suffix: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::Builder::new()
        .suffix(suffix)
        .tempfile()
        .expect("create Zeiss probe fixture");
    file.write_all(b"ZISRAWFILE\0\0\0\0\0\0")
        .expect("write Zeiss file magic");
    file.flush().expect("flush Zeiss probe fixture");

    file
}

#[test]
fn builtin_registry_recognizes_czi_magic() {
    let file = zeiss_magic_fixture(".czi");
    let detected = FormatRegistry::builtin()
        .detect_vendor(file.path())
        .expect("probe CZI fixture");
    assert_eq!(detected.map(|probe| probe.vendor), Some("zeiss".into()));
}
