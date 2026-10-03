use std::io::Write;

use tempfile::NamedTempFile;

use super::{parse_ini_file_with_budget, parse_ini_text_with_budget};
use crate::core::limits::SlideLimits;
use crate::core::registry::OpenBudget;
use crate::error::WsiError;

#[test]
fn parse_ini_text_trims_groups_keys_and_values() {
    let parsed = parse_ini_text_with_budget(
        r#"
            ; comment
            [ Group ]
            key = value
            padded =  spaced value
            # another comment
            ignored without equals
            "#,
        &OpenBudget::new(SlideLimits::default()),
    )
    .expect("parse INI text");

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

    let budget = OpenBudget::new(SlideLimits::default());
    let stripped = parse_ini_file_with_budget(
        file.path(),
        1024,
        |_| WsiError::UnsupportedFormat("too large".into()),
        true,
        &budget,
    )
    .expect("parse BOM-stripped INI");
    assert!(stripped.groups.contains_key("GENERAL"));

    let preserved = parse_ini_file_with_budget(
        file.path(),
        1024,
        |_| WsiError::UnsupportedFormat("too large".into()),
        false,
        &budget,
    )
    .expect("parse BOM-preserved INI");
    assert!(!preserved.groups.contains_key("GENERAL"));
}
