use super::storage::{hex_encode, is_fresh_svcache};
use super::*;
use std::io::ErrorKind;
use std::sync::Arc;

pub fn default_svcache_path(source_path: &Path) -> PathBuf {
    let name = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("slide");
    source_path.with_file_name(format!("{name}.svcache"))
}

pub fn cache_dir_svcache_path(source_path: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(source_path.to_string_lossy().as_bytes());
    let hash = hex_encode(&hasher.finalize());
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache")
        .join("slideviewer")
        .join("svcache")
        .join(format!("{hash}.svcache"))
}

pub fn svcache_candidate_paths(source_path: &Path) -> [PathBuf; 2] {
    [
        default_svcache_path(source_path),
        cache_dir_svcache_path(source_path),
    ]
}

pub(crate) fn resolve_open_path_with_policy(
    path: &Path,
    policy: SvcachePolicy,
) -> Result<PathBuf, WsiError> {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svcache"))
    {
        return Ok(path.to_path_buf());
    }
    if matches!(policy, SvcachePolicy::Off) {
        return Ok(path.to_path_buf());
    }

    let mut stale_candidate = None;
    for candidate in svcache_candidate_paths(path) {
        match std::fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() => match is_fresh_svcache(&candidate, path) {
                Ok(true) => return Ok(candidate),
                Ok(false) => stale_candidate = Some(candidate),
                Err(err) => return Err(err),
            },
            Ok(_) => continue,
            Err(source) if source.kind() == ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(WsiError::IoWithPath {
                    source: Arc::new(source),
                    path: candidate,
                });
            }
        }
    }

    if matches!(policy, SvcachePolicy::RequireFresh) {
        let detail = stale_candidate
            .map(|candidate| format!("; stale candidate: {}", candidate.display()))
            .unwrap_or_default();
        return Err(WsiError::UnsupportedFormat(format!(
            "fresh .svcache required for {}{}",
            path.display(),
            detail
        )));
    }
    Ok(path.to_path_buf())
}
