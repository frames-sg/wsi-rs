//! WSI pixel-contract validation using the J2K codec's parser.
use crate::core::limits::MAX_DECODED_IMAGE_BYTES;
use crate::WsiError;

#[derive(Debug)]
pub(crate) struct Jp2kCodestreamInfo {
    pub image_width: u32,
    pub image_height: u32,
    pub components: Vec<j2k::J2kComponentInfo>,
    pub multiple_component_transform: bool,
}

pub(crate) fn parse_codestream_header(data: &[u8]) -> Result<Jp2kCodestreamInfo, WsiError> {
    let view = j2k::J2kView::parse(data).map_err(|e| WsiError::Jp2k(e.to_string()))?;
    let metadata = view
        .support_info()
        .ok_or_else(|| WsiError::Jp2k("missing strict J2K component metadata".into()))?;
    Ok(Jp2kCodestreamInfo {
        image_width: metadata.info.dimensions.0,
        image_height: metadata.info.dimensions.1,
        components: metadata.components.clone(),
        multiple_component_transform: matches!(
            metadata.info.colorspace,
            j2k_core::Colorspace::Ict | j2k_core::Colorspace::Rct
        ),
    })
}

/// The WSI adapter promises unsigned RGB8 output. Coding styles, quantization,
/// packet organization, and tile parts are validated by j2k itself.
pub(crate) fn validate_pixel_contract(info: &Jp2kCodestreamInfo) -> Result<(), WsiError> {
    let decoded_bytes = u64::from(info.image_width)
        .checked_mul(u64::from(info.image_height))
        .and_then(|n| n.checked_mul(3))
        .unwrap_or(u64::MAX);
    if decoded_bytes > MAX_DECODED_IMAGE_BYTES {
        return Err(WsiError::ResourceLimit {
            resource: "decoded JP2K image",
            requested: decoded_bytes,
            limit: MAX_DECODED_IMAGE_BYTES,
        });
    }
    if info.components.len() != 3 || info.components.iter().any(|c| c.bit_depth != 8 || c.signed) {
        return Err(WsiError::Jp2k(
            "WSI JP2K output requires three unsigned 8-bit components".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "jp2k_codestream/tests/adapter.rs"]
mod adapter_tests;
#[cfg(test)]
#[path = "jp2k_codestream/tests/reference.rs"]
mod reference;
