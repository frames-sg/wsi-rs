use super::*;
use crate::decode::jpeg::lossless::{
    append_quantized_block_window, assemble_baseline_dct_image, compare_dct_coding_profile,
    dct_mcu_dimensions, DctProfileMismatch,
};

pub(super) struct NdpiDctRetileSegment {
    pub(super) native_row: u32,
    pub(super) crop_start_mcu: u32,
    pub(super) crop_mcus: u32,
    pub(super) image: JpegDctImage,
}

/// MCU width and height in pixels: 8 times the largest component sampling factor.
pub(super) fn ndpi_dct_mcu_dimensions(image: &JpegDctImage) -> Result<(u32, u32), WsiError> {
    dct_mcu_dimensions(image).ok_or_else(|| WsiError::Unsupported {
        reason: "NDPI raw JPEG retile requires nonzero MCU dimensions".into(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_ndpi_retiled_dct_image(
    req: &TileViewRequest,
    content_width: u32,
    content_height: u32,
    native_col_start: u32,
    native_col_end: u32,
    native_row_start: u32,
    native_row_end: u32,
    segments: &[NdpiDctRetileSegment],
) -> Result<JpegDctImage, WsiError> {
    let Some(first) = segments.first().map(|segment| &segment.image) else {
        return Err(WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "NDPI raw JPEG retile found no source segments".into(),
        });
    };
    if first.coding_mode != JpegDctCodingMode::BaselineSequential {
        return Err(WsiError::Unsupported {
            reason: "NDPI raw JPEG retile supports baseline sequential JPEG only".into(),
        });
    }
    let component_count = first.components.len();
    if component_count != 1 && component_count != 3 {
        return Err(WsiError::Unsupported {
            reason: format!(
                "NDPI raw JPEG retile supports 1 or 3 components, got {component_count}"
            ),
        });
    }
    let (mcu_width, mcu_height) = ndpi_dct_mcu_dimensions(first)?;
    let output_mcu_cols = content_width.div_ceil(mcu_width);
    let output_mcu_rows = content_height.div_ceil(mcu_height);
    let expected_segments_per_row = native_col_end - native_col_start + 1;
    let mut component_blocks = (0..component_count)
        .map(|idx| {
            let component = &first.components[idx];
            let capacity = output_mcu_cols
                .saturating_mul(output_mcu_rows)
                .saturating_mul(u32::from(component.h_samp))
                .saturating_mul(u32::from(component.v_samp));
            Vec::with_capacity(capacity as usize)
        })
        .collect::<Vec<_>>();

    for native_row in native_row_start..=native_row_end {
        let row_segments = segments
            .iter()
            .filter(|segment| segment.native_row == native_row)
            .collect::<Vec<_>>();
        if row_segments.len() != expected_segments_per_row as usize {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "NDPI raw JPEG retile row {native_row} has {} source segments, expected {expected_segments_per_row}",
                    row_segments.len()
                ),
            });
        }

        for (component_idx, blocks) in component_blocks
            .iter_mut()
            .enumerate()
            .take(component_count)
        {
            let reference = &first.components[component_idx];
            let h = u32::from(reference.h_samp);
            let v = u32::from(reference.v_samp);
            for block_y in 0..v {
                let row_start_len = blocks.len();
                for segment in &row_segments {
                    validate_ndpi_retile_segment(first, &segment.image)?;
                    let component = &segment.image.components[component_idx];
                    if component.block_rows != v {
                        return Err(WsiError::Unsupported {
                            reason: format!(
                                "NDPI raw JPEG retile expects one MCU row per source segment, got {} block rows for component {component_idx}",
                                component.block_rows
                            ),
                        });
                    }
                    let (Some(block_x), Some(block_count)) = (
                        segment.crop_start_mcu.checked_mul(h),
                        segment.crop_mcus.checked_mul(h),
                    ) else {
                        return Err(WsiError::Unsupported {
                            reason: "NDPI raw JPEG retile block range overflow".into(),
                        });
                    };
                    append_quantized_block_window(
                        component,
                        block_x,
                        block_y,
                        block_count,
                        1,
                        blocks,
                    )
                    .map_err(|_| WsiError::Unsupported {
                        reason: format!(
                            "NDPI raw JPEG retile crop exceeds component {component_idx} block grid"
                        ),
                    })?;
                }
                let copied = blocks.len() - row_start_len;
                let expected = (output_mcu_cols * h) as usize;
                if copied != expected {
                    return Err(WsiError::Unsupported {
                        reason: format!(
                            "NDPI raw JPEG retile copied {copied} blocks for component {component_idx}, expected {expected}"
                        ),
                    });
                }
            }
        }
    }

    assemble_baseline_dct_image(first, content_width, content_height, component_blocks)
}

pub(super) fn validate_ndpi_retile_segment(
    reference: &JpegDctImage,
    candidate: &JpegDctImage,
) -> Result<(), WsiError> {
    if candidate.coding_mode != JpegDctCodingMode::BaselineSequential {
        return Err(WsiError::Unsupported {
            reason: "NDPI raw JPEG retile supports baseline sequential JPEG only".into(),
        });
    }
    compare_dct_coding_profile(reference, candidate).map_err(|mismatch| match mismatch {
        DctProfileMismatch::ColorProfile => WsiError::Unsupported {
            reason: "NDPI raw JPEG retile source segment color profile changed".into(),
        },
        DctProfileMismatch::Component(idx) => WsiError::Unsupported {
            reason: format!(
                "NDPI raw JPEG retile source segment component {idx} coding profile changed"
            ),
        },
    })
}
