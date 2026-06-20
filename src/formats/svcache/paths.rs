use super::storage::{hex_encode, is_fresh_svcache};
use super::*;

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

    for candidate in svcache_candidate_paths(path) {
        if candidate.is_file() && is_fresh_svcache(&candidate, path).unwrap_or(false) {
            return Ok(candidate);
        }
    }

    if matches!(policy, SvcachePolicy::RequireFresh) {
        return Err(WsiError::UnsupportedFormat(format!(
            "fresh .svcache required for {}",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}
