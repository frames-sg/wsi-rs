use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::core::limits::read_file_bounded;
use crate::core::registry::OpenBudget;
use crate::error::WsiError;

#[derive(Default)]
pub(crate) struct ParsedIni {
    pub(crate) groups: HashMap<String, HashMap<String, String>>,
}

#[cfg(test)]
pub(crate) fn parse_ini_file(
    path: &Path,
    max_size: u64,
    too_large: impl FnOnce(&Path) -> WsiError,
    strip_utf8_bom: bool,
) -> Result<ParsedIni, WsiError> {
    let bytes = match read_file_bounded(path, max_size, "INI file") {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::InvalidData => {
            return Err(too_large(path));
        }
        Err(source) => {
            return Err(WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            });
        }
    };
    let text = String::from_utf8(bytes).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(std::io::Error::new(std::io::ErrorKind::InvalidData, source)),
        path: path.to_path_buf(),
    })?;
    let text = if strip_utf8_bom {
        text.strip_prefix('\u{feff}').unwrap_or(&text)
    } else {
        &text
    };
    Ok(parse_ini_text(text))
}

pub(crate) fn parse_ini_file_with_budget(
    path: &Path,
    structural_max_size: u64,
    too_large: impl FnOnce(&Path) -> WsiError,
    strip_utf8_bom: bool,
    budget: &OpenBudget,
) -> Result<ParsedIni, WsiError> {
    let max_size = structural_max_size.min(budget.limits().aggregate_metadata_bytes());
    let bytes = match read_file_bounded(path, max_size, "INI file") {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::InvalidData => {
            return Err(too_large(path));
        }
        Err(source) => {
            return Err(WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.to_path_buf(),
            });
        }
    };
    let text = String::from_utf8(bytes).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(std::io::Error::new(std::io::ErrorKind::InvalidData, source)),
        path: path.to_path_buf(),
    })?;
    let text = if strip_utf8_bom {
        text.strip_prefix('\u{feff}').unwrap_or(&text)
    } else {
        &text
    };
    parse_ini_text_with_budget(text, budget)
}

fn parse_ini_text_with_budget(text: &str, budget: &OpenBudget) -> Result<ParsedIni, WsiError> {
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
            let bytes = u64::try_from(group.len()).unwrap_or(u64::MAX);
            budget.check_metadata_value(bytes)?;
            parsed
                .groups
                .try_reserve(1)
                .map_err(|_| WsiError::ResourceLimit {
                    resource: "aggregate metadata",
                    requested: budget.limits().aggregate_metadata_bytes(),
                    limit: budget.limits().aggregate_metadata_bytes(),
                })?;
            budget.retain_metadata(bytes)?;
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
        let key = key.trim();
        let value = value.trim();
        let key_bytes = u64::try_from(key.len()).unwrap_or(u64::MAX);
        let value_bytes = u64::try_from(value.len()).unwrap_or(u64::MAX);
        budget.check_metadata_value(key_bytes)?;
        budget.check_metadata_value(value_bytes)?;
        let values = parsed
            .groups
            .get_mut(group)
            .expect("current INI group exists");
        values.try_reserve(1).map_err(|_| WsiError::ResourceLimit {
            resource: "aggregate metadata",
            requested: budget.limits().aggregate_metadata_bytes(),
            limit: budget.limits().aggregate_metadata_bytes(),
        })?;
        budget.retain_metadata(key_bytes)?;
        budget.retain_metadata(value_bytes)?;
        values.insert(key.to_string(), value.to_string());
    }
    Ok(parsed)
}

#[cfg(test)]
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
mod tests;
