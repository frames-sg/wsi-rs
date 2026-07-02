use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::error::WsiError;

#[derive(Default)]
pub(crate) struct ParsedIni {
    pub(crate) groups: HashMap<String, HashMap<String, String>>,
}

pub(crate) fn parse_ini_file(
    path: &Path,
    max_size: u64,
    too_large: impl FnOnce(&Path) -> WsiError,
    strip_utf8_bom: bool,
) -> Result<ParsedIni, WsiError> {
    let metadata = std::fs::metadata(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    if metadata.len() > max_size {
        return Err(too_large(path));
    }
    let text = std::fs::read_to_string(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    let text = if strip_utf8_bom {
        text.strip_prefix('\u{feff}').unwrap_or(&text)
    } else {
        &text
    };
    Ok(parse_ini_text(text))
}

fn parse_ini_text(text: &str) -> ParsedIni {
    let mut parsed = ParsedIni::default();
    let mut current_group: Option<String> = None;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(group) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            current_group = Some(group.to_string());
            parsed.groups.entry(group.to_string()).or_default();
            continue;
        }
        let Some(group) = current_group.as_ref() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        parsed
            .groups
            .entry(group.clone())
            .or_default()
            .insert(key.trim().to_string(), value.trim().to_string());
    }
    parsed
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::{parse_ini_file, parse_ini_text};
    use crate::error::WsiError;

    #[test]
    fn parse_ini_text_trims_groups_keys_and_values() {
        let parsed = parse_ini_text(
            r#"
            ; comment
            [ Group ]
            key = value
            padded =  spaced value
            # another comment
            ignored without equals
            "#,
        );

        let group = parsed.groups.get(" Group ").expect("group");
        assert_eq!(group.get("key").map(String::as_str), Some("value"));
        assert_eq!(
            group.get("padded").map(String::as_str),
            Some("spaced value")
        );
    }

    #[test]
    fn parse_ini_file_strips_utf8_bom_only_when_requested() {
        let mut file = NamedTempFile::new().expect("temporary INI file");
        write!(file, "\u{feff}[GENERAL]\nKEY=VALUE\n").expect("write INI");

        let stripped = parse_ini_file(
            file.path(),
            1024,
            |_| WsiError::UnsupportedFormat("too large".into()),
            true,
        )
        .expect("parse BOM-stripped INI");
        assert!(stripped.groups.contains_key("GENERAL"));

        let preserved = parse_ini_file(
            file.path(),
            1024,
            |_| WsiError::UnsupportedFormat("too large".into()),
            false,
        )
        .expect("parse BOM-preserved INI");
        assert!(!preserved.groups.contains_key("GENERAL"));
    }
}
