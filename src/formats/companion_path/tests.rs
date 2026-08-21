use super::*;

#[test]
fn accepts_existing_file_below_root() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("data")).expect("data directory");
    std::fs::write(root.path().join("data/tile.jpg"), b"tile").expect("tile");
    let path = resolve_companion_file(&root.path().join("slide.ini"), root.path(), "data/tile.jpg")
        .expect("valid companion");
    assert!(path.ends_with("data/tile.jpg"));
}

#[test]
fn rejects_absolute_and_parent_paths() {
    let root = tempfile::tempdir().expect("root");
    for value in ["../secret", "/tmp/secret"] {
        assert!(
            resolve_companion_file(&root.path().join("slide.ini"), root.path(), value).is_err()
        );
    }
}

#[cfg(unix)]
#[test]
fn rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("secret"), b"secret").expect("secret");
    symlink(outside.path().join("secret"), root.path().join("escape")).expect("symlink");

    assert!(resolve_companion_file(&root.path().join("slide.ini"), root.path(), "escape").is_err());
}
