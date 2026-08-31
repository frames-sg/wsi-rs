use super::*;

// ── Detection helper ────────────────────────────────────────────────

/// Check if an IFD has an XMP tag containing `<iScan`.
pub(super) fn has_iscan_xmp(container: &TiffContainer, ifd_id: IfdId) -> bool {
    // Try get_string first (type ASCII), fall back to get_bytes (type BYTE/Undefined).
    if let Ok(s) = container.get_string(ifd_id, tags::XMP) {
        return s.contains("iScan");
    }
    if let Ok(bytes) = container.get_bytes(ifd_id, tags::XMP) {
        if let Ok(s) = std::str::from_utf8(bytes) {
            return s.contains("iScan");
        }
        // Byte-level search as last resort.
        return bytes.windows(b"iScan".len()).any(|w| w == b"iScan");
    }
    false
}

// ── XMP parsing ─────────────────────────────────────────────────────

/// Find the first XMP tag across all top-level IFDs and return it as a string.
pub(super) fn find_xmp_string(container: &TiffContainer) -> Result<Option<String>, TiffParseError> {
    for &ifd_id in container.top_ifds() {
        if let Ok(s) = container.get_string(ifd_id, tags::XMP) {
            if let Some(xmp) = extract_iscan_fragment(s) {
                return Ok(Some(xmp));
            }
        }
        if let Ok(bytes) = container.get_bytes(ifd_id, tags::XMP) {
            if let Some(xmp) = extract_iscan_fragment_bytes(bytes) {
                return Ok(Some(xmp));
            }
            if let Ok(s) = std::str::from_utf8(bytes) {
                if let Some(xmp) = extract_iscan_fragment(s) {
                    return Ok(Some(xmp));
                }
            }
        }
    }
    Ok(None)
}

/// Parse iScan attributes into vendor properties.
pub(super) fn parse_iscan_properties(xmp: &str, properties: &mut Properties) {
    for (key, value) in parse_iscan_attributes(xmp) {
        if !value.is_empty() {
            properties.insert(format!("ventana.{key}"), value);
        }
    }

    if let Some(mag) = properties
        .get("ventana.Magnification")
        .map(|s| s.to_string())
    {
        if let Ok(power) = mag.parse::<f64>() {
            if power.is_finite() && power > 0.0 && power <= u32::MAX as f64 {
                properties.insert("openslide.objective-power", format!("{}", power as u32));
            }
        }
    }
    if let Some(res) = properties.get("ventana.ScanRes").map(|s| s.to_string()) {
        if res
            .parse::<f64>()
            .is_ok_and(|value| value.is_finite() && value > 0.0)
        {
            properties.insert("openslide.mpp-x", res.clone());
            properties.insert("openslide.mpp-y", res);
        }
    }
}

fn parse_iscan_attributes(xmp: &str) -> Vec<(String, String)> {
    let start = match xmp.find("<iScan") {
        Some(pos) => pos + "<iScan".len(),
        None => return Vec::new(),
    };
    let end = match xmp[start..].find('>') {
        Some(pos) => start + pos,
        None => return Vec::new(),
    };
    let mut attrs = Vec::new();
    let mut rest = xmp[start..end].trim();

    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() || rest.starts_with('/') {
            break;
        }
        let Some(eq_idx) = rest.find('=') else {
            break;
        };
        let key = rest[..eq_idx].trim();
        if key.is_empty() {
            break;
        }
        let mut value_rest = rest[eq_idx + 1..].trim_start();
        let Some(quote) = value_rest.chars().next() else {
            break;
        };
        if quote != '"' && quote != '\'' {
            break;
        }
        value_rest = &value_rest[quote.len_utf8()..];
        let Some(close_idx) = value_rest.find(quote) else {
            break;
        };
        attrs.push((key.to_string(), value_rest[..close_idx].to_string()));
        rest = &value_rest[close_idx + quote.len_utf8()..];
    }

    attrs
}

// ── EncodeInfo XML discovery ────────────────────────────────────────

/// Search IFD data for `<EncodeInfo>` XML containing the tile layout.
/// Ventana stores this in one of the IFDs (typically as XMP or ImageDescription,
/// or sometimes embedded in the strip/tile data). We search all IFDs for any
/// tag payload that contains an `EncodeInfo` fragment.
pub(super) fn find_encode_info_xml(container: &TiffContainer) -> Result<String, TiffParseError> {
    for &ifd_id in container.top_ifds() {
        for &tag in &[tags::IMAGE_DESCRIPTION, tags::XMP] {
            if let Ok(bytes) = container.get_bytes(ifd_id, tag) {
                if let Some(xml) = extract_encode_info_bytes(bytes) {
                    return Ok(xml);
                }
                if let Ok(s) = std::str::from_utf8(bytes) {
                    if let Some(xml) = extract_encode_info(s) {
                        return Ok(xml);
                    }
                }
            }
        }
    }
    Err(TiffParseError::Structure(
        "Ventana BIF: no EncodeInfo XML found".into(),
    ))
}

/// Extract `<EncodeInfo>...</EncodeInfo>` from a larger string.
pub(super) fn extract_encode_info(s: &str) -> Option<String> {
    extract_xml_fragment(s, "<EncodeInfo", "</EncodeInfo>")
}

/// Extract `<EncodeInfo ...>...</EncodeInfo>` from raw tag bytes.
pub(super) fn extract_encode_info_bytes(bytes: &[u8]) -> Option<String> {
    extract_xml_fragment_bytes(bytes, b"<EncodeInfo", b"</EncodeInfo>")
}

/// Extract `<iScan .../>` or `<iScan ...></iScan>` from a larger string.
fn extract_iscan_fragment(s: &str) -> Option<String> {
    extract_xml_fragment_with_optional_self_closing(s, "<iScan", "</iScan>")
}

/// Extract `<iScan .../>` or `<iScan ...></iScan>` from raw tag bytes.
pub(super) fn extract_iscan_fragment_bytes(bytes: &[u8]) -> Option<String> {
    extract_xml_fragment_with_optional_self_closing_bytes(bytes, b"<iScan", b"</iScan>")
}

fn extract_xml_fragment(s: &str, start_tag_prefix: &str, end_tag: &str) -> Option<String> {
    let start = s.find(start_tag_prefix)?;
    let end = s[start..].find(end_tag)? + start + end_tag.len();
    Some(s[start..end].to_string())
}

fn extract_xml_fragment_bytes(
    bytes: &[u8],
    start_tag_prefix: &[u8],
    end_tag: &[u8],
) -> Option<String> {
    let start = find_bytes(bytes, start_tag_prefix)?;
    let end = find_bytes(&bytes[start..], end_tag)? + start + end_tag.len();
    Some(String::from_utf8_lossy(&bytes[start..end]).into_owned())
}

fn extract_xml_fragment_with_optional_self_closing(
    s: &str,
    start_tag_prefix: &str,
    end_tag: &str,
) -> Option<String> {
    let start = s.find(start_tag_prefix)?;
    let fragment = &s[start..];
    let end = fragment.find("/>").map(|pos| start + pos + 2).or_else(|| {
        fragment
            .find(end_tag)
            .map(|pos| start + pos + end_tag.len())
    })?;
    Some(s[start..end].to_string())
}

fn extract_xml_fragment_with_optional_self_closing_bytes(
    bytes: &[u8],
    start_tag_prefix: &[u8],
    end_tag: &[u8],
) -> Option<String> {
    let start = find_bytes(bytes, start_tag_prefix)?;
    let fragment = &bytes[start..];
    let end = find_bytes(fragment, b"/>")
        .map(|pos| start + pos + 2)
        .or_else(|| find_bytes(fragment, end_tag).map(|pos| start + pos + end_tag.len()))?;
    Some(String::from_utf8_lossy(&bytes[start..end]).into_owned())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }

    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    let mut idx = 0;

    while idx <= last_start {
        let offset = haystack[idx..=last_start]
            .iter()
            .position(|&byte| byte == first)?;
        let candidate = idx + offset;
        if haystack[candidate..candidate + needle.len()] == *needle {
            return Some(candidate);
        }
        idx = candidate + 1;
    }

    None
}
