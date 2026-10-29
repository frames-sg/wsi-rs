use super::*;
#[cfg(test)]
pub(super) use crate::decode::jpeg::frame::jpeg_segment_color_hint;
pub(super) use crate::decode::jpeg::frame::{
    disable_jpeg_restart_interval, jpeg_bitstream_color_hint, parse_baseline_jpeg_frame_info,
    patch_jpeg_sof0_dimensions, standalone_jpeg_frame_owned, strip_leading_restart_marker,
    strip_trailing_eoi_marker, strip_trailing_restart_marker, JpegBitstreamColorHint,
};

pub(super) fn j2k_decode_options(color_transform: J2kColorTransform) -> J2kDecodeOptions {
    J2kDecodeOptions::default().with_color_transform(color_transform)
}

pub(super) fn tiff_jpeg_color_transform(
    photometric: u32,
    samples_per_pixel: u32,
    bitstream_hint: JpegBitstreamColorHint,
) -> J2kColorTransform {
    if samples_per_pixel == 3 {
        match bitstream_hint {
            JpegBitstreamColorHint::Rgb => return J2kColorTransform::ForceRgb,
            JpegBitstreamColorHint::RgbComponentIds012 if photometric != 6 => {
                return J2kColorTransform::ForceRgb;
            }
            JpegBitstreamColorHint::YCbCr => return J2kColorTransform::ForceYCbCr,
            JpegBitstreamColorHint::RgbComponentIds012 | JpegBitstreamColorHint::Unknown => {}
        }
    }

    match (photometric, samples_per_pixel) {
        (2, 3) => J2kColorTransform::ForceRgb,
        (6, 3) => J2kColorTransform::ForceYCbCr,
        _ => J2kColorTransform::Auto,
    }
}

pub(super) fn tiff_jpeg_encoded_photometric_interpretation(
    photometric: u32,
    samples_per_pixel: u32,
    bitstream_hint: JpegBitstreamColorHint,
) -> Result<EncodedTilePhotometricInterpretation, WsiError> {
    match samples_per_pixel {
        // TIFF MinIsWhite: 0 is white, as DICOM MONOCHROME1.
        1 if photometric == 0 => Ok(EncodedTilePhotometricInterpretation::Monochrome1),
        1 => Ok(EncodedTilePhotometricInterpretation::Monochrome2),
        3 => match tiff_jpeg_color_transform(photometric, samples_per_pixel, bitstream_hint) {
            J2kColorTransform::ForceRgb => Ok(EncodedTilePhotometricInterpretation::Rgb),
            J2kColorTransform::ForceYCbCr => Ok(EncodedTilePhotometricInterpretation::YbrFull422),
            J2kColorTransform::Auto => match bitstream_hint {
                JpegBitstreamColorHint::Rgb | JpegBitstreamColorHint::RgbComponentIds012 => {
                    Ok(EncodedTilePhotometricInterpretation::Rgb)
                }
                JpegBitstreamColorHint::YCbCr | JpegBitstreamColorHint::Unknown => {
                    Ok(EncodedTilePhotometricInterpretation::YbrFull422)
                }
            },
            other => Err(WsiError::Unsupported {
                reason: format!("unsupported JPEG color transform {other:?}"),
            }),
        },
        other => Err(WsiError::Unsupported {
            reason: format!("JPEG passthrough supports 1 or 3 components, got {other}"),
        }),
    }
}

pub(super) fn j2k_downscale_for_factor(factor: u32) -> Option<J2kDownscale> {
    match factor {
        1 => Some(J2kDownscale::None),
        2 => Some(J2kDownscale::Half),
        4 => Some(J2kDownscale::Quarter),
        8 => Some(J2kDownscale::Eighth),
        _ => None,
    }
}
