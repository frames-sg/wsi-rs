use super::support::*;
use std::fs;

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
fn public_docs_do_not_advertise_unregistered_zeiss_support() {
    let formats_mod =
        fs::read_to_string(crate_root().join("src/formats/mod.rs")).expect("read formats mod");
    let registry = read_repo_text("src/core/registry");
    let zeiss_registered = formats_mod.contains("mod zeiss") && registry.contains("ZeissBackend");

    if !zeiss_registered {
        let docs = [(
            "README.md",
            fs::read_to_string(crate_root().join("README.md")).expect("read README"),
        )];
        let offenders = docs
            .iter()
            .filter_map(|(path, text)| text.contains("Zeiss").then_some(*path))
            .collect::<Vec<_>>();
        assert!(
            offenders.is_empty(),
            "public docs must not advertise Zeiss until the backend is registered: {}",
            offenders.join(", ")
        );
    }
}

#[test]
fn unregistered_zeiss_backend_is_not_left_as_packaged_source() {
    let zeiss_source = crate_root().join("src/formats/zeiss.rs");
    if !zeiss_source.exists() {
        return;
    }

    let formats_mod =
        fs::read_to_string(crate_root().join("src/formats/mod.rs")).expect("read formats mod");
    let registry = read_repo_text("src/core/registry");
    assert!(
        formats_mod.contains("mod zeiss") && registry.contains("ZeissBackend"),
        "src/formats/zeiss.rs exists but the Zeiss backend is not registered"
    );
}
