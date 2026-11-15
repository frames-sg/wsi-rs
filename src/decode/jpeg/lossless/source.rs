//! Header facts of stacked lossless-crop sources that are matched before any
//! part is entropy-decoded, and the source markers a stacked crop carries
//! over. j2k-jpeg walks the marker segments and parses the frame header; it
//! does not report JFIF/Adobe signalling, which is read here.

use j2k_jpeg::{
    is_sof_marker, iter_segments, parse_sof_info, JpegError, JpegSegment, JpegSofInfo, SofKind,
};

use super::{check_blocks_per_mcu, CropFailure};
use crate::error::WsiError;

/// Whether a segment is JFIF APP0 or Adobe APP14 color signalling.
fn is_color_app_segment(segment: &JpegSegment<'_>) -> bool {
    match segment.marker {
        0xE0 => segment.payload.starts_with(b"JFIF\0"),
        0xEE => segment.payload.len() >= 12 && segment.payload.starts_with(b"Adobe"),
        _ => false,
    }
}

/// Source markers that decide how decoders interpret the components.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ColorSignalling {
    pub(super) jfif: bool,
    pub(super) adobe_transform: Option<u8>,
}

/// The parts of one standalone JPEG header a stacked crop matches on.
#[derive(Debug, Clone)]
pub(super) struct SourceFrame {
    /// The frame header before the first scan, as j2k-jpeg parses it.
    pub(super) sof: JpegSofInfo,
    pub(super) color: ColorSignalling,
}

fn header_error(err: JpegError) -> WsiError {
    CropFailure::from_decode(err).into_error()
}

impl SourceFrame {
    pub(super) fn parse(data: &[u8]) -> Result<Self, WsiError> {
        let mut sof = None;
        let mut color = ColorSignalling::default();
        for segment in iter_segments(data) {
            let segment = segment.map_err(header_error)?;
            match segment.marker {
                0xDA => break,
                marker if is_sof_marker(marker) && sof.is_none() => {
                    sof = Some(parse_sof_info(marker, segment.payload).map_err(header_error)?);
                }
                0xE0 if is_color_app_segment(&segment) => color.jfif = true,
                0xEE if is_color_app_segment(&segment) => {
                    color.adobe_transform.get_or_insert(segment.payload[11]);
                }
                _ => {}
            }
        }
        let sof = sof.ok_or_else(|| {
            WsiError::Jpeg("lossless JPEG crop input has no frame before its first scan".into())
        })?;
        if !matches!(sof.sof_kind, SofKind::Baseline8 | SofKind::Progressive8)
            || !matches!(sof.component_ids.len(), 1 | 3)
        {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "lossless JPEG crop cuts 8-bit baseline and progressive JPEG of 1 or 3 components, got {:?} with {} components",
                    sof.sof_kind,
                    sof.component_ids.len()
                ),
            });
        }
        check_blocks_per_mcu(sof.sampling.components())?;
        Ok(Self { sof, color })
    }

    /// Copies of `data`'s complete JFIF APP0 and Adobe APP14 segments, in
    /// source order, for the crop to carry over.
    pub(super) fn color_app_segments(data: &[u8]) -> Result<Vec<u8>, WsiError> {
        let mut segments = Vec::new();
        for segment in iter_segments(data) {
            let segment = segment.map_err(header_error)?;
            if segment.marker == 0xDA {
                break;
            }
            if is_color_app_segment(&segment) {
                let end = segment.payload_offset + segment.payload.len();
                segments.extend_from_slice(&data[segment.marker_offset..end]);
            }
        }
        Ok(segments)
    }

    pub(super) fn width(&self) -> u32 {
        u32::from(self.sof.dimensions.0)
    }

    pub(super) fn height(&self) -> u32 {
        u32::from(self.sof.dimensions.1)
    }

    /// MCU width and height in pixels.
    pub(super) fn mcu_dimensions(&self) -> (u32, u32) {
        let sampling = &self.sof.sampling;
        (u32::from(sampling.max_h) * 8, u32::from(sampling.max_v) * 8)
    }

    /// Number of 8x8 blocks of each component in one MCU.
    pub(super) fn mcu_blocks(&self) -> impl Iterator<Item = usize> + '_ {
        let components = self.sof.sampling.components().iter();
        components.map(|&(h, v)| usize::from(h) * usize::from(v))
    }
}
