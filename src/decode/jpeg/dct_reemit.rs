//! Coefficient-domain baseline JPEG re-emission, delegated to j2k-jpeg.
//!
//! j2k-jpeg owns the entropy coder, Annex K Huffman tables, quantization
//! table slots and frame layout. This module only chooses the signalling a
//! WSI re-emission carries and maps codec errors onto [`WsiError`].

use j2k_jpeg::transcode::{
    encode_baseline_dct_image_with_options, BaselineDctEncodeOptions, JpegDctImage,
};
use j2k_jpeg::JpegEncodeError;

use crate::error::WsiError;

/// Component identifiers j2k-jpeg writes for a frame it lays out itself.
const NUMERIC_COMPONENT_IDS: [u8; 3] = [1, 2, 3];

/// Re-emit quantized DCT blocks as a baseline JPEG with numeric component IDs
/// and an Adobe APP14 marker that states the resolved color transform, so
/// 4:4:4 YCbCr, whose sampling alone is ambiguous, keeps its meaning. Each
/// component keeps its own quant table.
pub(crate) fn encode_baseline_dct_image(image: &JpegDctImage) -> Result<Vec<u8>, WsiError> {
    let component_count = image.components.len();
    let adobe_transform = match (component_count, image.color_space) {
        (1, j2k_jpeg::ColorSpace::Grayscale) => None,
        (3, j2k_jpeg::ColorSpace::Rgb) => Some(0),
        (3, j2k_jpeg::ColorSpace::YCbCr) => Some(1),
        _ => {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "DCT JPEG re-emission supports gray, RGB or YCbCr, got {:?} with {component_count} components",
                    image.color_space
                ),
            });
        }
    };
    let app_segments = adobe_transform.map(|transform| {
        [
            0xFF, 0xEE, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, transform,
        ]
    });
    encode(
        image,
        BaselineDctEncodeOptions {
            app_segments: app_segments
                .as_ref()
                .map_or(&[], |segment| segment.as_slice()),
            component_ids: &NUMERIC_COMPONENT_IDS[..component_count],
        },
    )
}

/// Source signalling that a coefficient-domain re-emission carries over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DctFrameMarkers {
    /// Complete APPn marker segments (marker, length, payload), such as JFIF
    /// APP0 and Adobe APP14, written right after SOI.
    pub(crate) app_segments: Vec<u8>,
    /// SOF component identifiers in declaration order.
    pub(crate) component_ids: Vec<u8>,
}

/// Re-emit quantized DCT blocks as a baseline JPEG that keeps the source's
/// signalling: `markers.app_segments` follow SOI, SOF and SOS use
/// `markers.component_ids`, and every component keeps its own quant table, so
/// distinct Cb and Cr tables survive. Huffman tables are the JPEG Annex K
/// defaults and no restart interval is written.
pub(crate) fn encode_baseline_dct_image_with_markers(
    image: &JpegDctImage,
    markers: &DctFrameMarkers,
) -> Result<Vec<u8>, WsiError> {
    encode(
        image,
        BaselineDctEncodeOptions {
            app_segments: &markers.app_segments,
            component_ids: &markers.component_ids,
        },
    )
}

fn encode(
    image: &JpegDctImage,
    options: BaselineDctEncodeOptions<'_>,
) -> Result<Vec<u8>, WsiError> {
    encode_baseline_dct_image_with_options(image, options).map_err(|error| match error {
        JpegEncodeError::InvalidDctImage { reason } => WsiError::Unsupported {
            reason: format!("DCT JPEG re-emission rejected the coefficient image: {reason}"),
        },
        error => WsiError::Jpeg(format!("DCT JPEG re-emission failed: {error}")),
    })
}
