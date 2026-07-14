use super::*;

pub(super) struct NdpiDctRetileSegment {
    pub(super) native_row: u32,
    pub(super) crop_start_mcu: u32,
    pub(super) crop_mcus: u32,
    pub(super) image: JpegDctImage,
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
    let max_h = first
        .components
        .iter()
        .map(|component| component.h_samp)
        .max()
        .unwrap_or(1);
    let max_v = first
        .components
        .iter()
        .map(|component| component.v_samp)
        .max()
        .unwrap_or(1);
    let mcu_width = u32::from(max_h) * 8;
    let mcu_height = u32::from(max_v) * 8;
    if mcu_width == 0 || mcu_height == 0 {
        return Err(WsiError::Unsupported {
            reason: "NDPI raw JPEG retile requires nonzero MCU dimensions".into(),
        });
    }
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
                    let start_block = block_y * component.block_cols + segment.crop_start_mcu * h;
                    let block_count = segment.crop_mcus * h;
                    let end_block = start_block.checked_add(block_count).ok_or_else(|| {
                        WsiError::Unsupported {
                            reason: "NDPI raw JPEG retile block range overflow".into(),
                        }
                    })?;
                    let start = start_block as usize;
                    let end = end_block as usize;
                    if end > component.quantized_blocks.len() {
                        return Err(WsiError::Unsupported {
                            reason: format!(
                                "NDPI raw JPEG retile crop exceeds component {component_idx} block grid"
                            ),
                        });
                    }
                    blocks.extend_from_slice(&component.quantized_blocks[start..end]);
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

    let components = first
        .components
        .iter()
        .enumerate()
        .map(|(idx, component)| JpegDctComponent {
            component_index: idx,
            width: content_width
                .saturating_mul(u32::from(component.h_samp))
                .div_ceil(u32::from(max_h)),
            height: content_height
                .saturating_mul(u32::from(component.v_samp))
                .div_ceil(u32::from(max_v)),
            h_samp: component.h_samp,
            v_samp: component.v_samp,
            block_cols: output_mcu_cols * u32::from(component.h_samp),
            block_rows: output_mcu_rows * u32::from(component.v_samp),
            quant_table: component.quant_table,
            quantized_blocks: std::mem::take(&mut component_blocks[idx]),
            dequantized_blocks: Vec::new(),
        })
        .collect();

    Ok(JpegDctImage {
        width: content_width,
        height: content_height,
        color_space: first.color_space,
        coding_mode: JpegDctCodingMode::BaselineSequential,
        scan_count: 1,
        components,
        restart_index: None,
    })
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
    if candidate.color_space != reference.color_space
        || candidate.components.len() != reference.components.len()
    {
        return Err(WsiError::Unsupported {
            reason: "NDPI raw JPEG retile source segment color profile changed".into(),
        });
    }
    for (idx, (expected, actual)) in reference
        .components
        .iter()
        .zip(candidate.components.iter())
        .enumerate()
    {
        if expected.h_samp != actual.h_samp
            || expected.v_samp != actual.v_samp
            || expected.quant_table != actual.quant_table
        {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "NDPI raw JPEG retile source segment component {idx} coding profile changed"
                ),
            });
        }
    }
    Ok(())
}
