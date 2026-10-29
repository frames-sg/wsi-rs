//! Block-grid helpers shared by coefficient-domain JPEG crops and retiles.

use j2k_jpeg::transcode::{JpegDctCodingMode, JpegDctComponent, JpegDctImage};

use crate::error::WsiError;

/// MCU width and height in pixels: 8 times the largest component sampling
/// factor in each direction. `None` when a sampling factor is zero.
pub(crate) fn dct_mcu_dimensions(image: &JpegDctImage) -> Option<(u32, u32)> {
    let max_h = image
        .components
        .iter()
        .map(|component| component.h_samp)
        .max()
        .unwrap_or(1);
    let max_v = image
        .components
        .iter()
        .map(|component| component.v_samp)
        .max()
        .unwrap_or(1);
    let mcu_width = u32::from(max_h) * 8;
    let mcu_height = u32::from(max_v) * 8;
    (mcu_width != 0 && mcu_height != 0).then_some((mcu_width, mcu_height))
}

/// A block window that does not lie inside the component's block grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DctWindowOutOfRange;

/// Append the `cols` x `rows` window of `component`'s quantized blocks whose
/// top-left block is (`block_x`, `block_y`) to `dst`, row by row. `dst` is
/// left unchanged when the window leaves the component's block grid.
pub(crate) fn append_quantized_block_window(
    component: &JpegDctComponent,
    block_x: u32,
    block_y: u32,
    cols: u32,
    rows: u32,
    dst: &mut Vec<[i16; 64]>,
) -> Result<(), DctWindowOutOfRange> {
    let x_end = block_x.checked_add(cols).ok_or(DctWindowOutOfRange)?;
    let y_end = block_y.checked_add(rows).ok_or(DctWindowOutOfRange)?;
    let grid_blocks = (component.block_cols as usize)
        .checked_mul(component.block_rows as usize)
        .ok_or(DctWindowOutOfRange)?;
    if x_end > component.block_cols
        || y_end > component.block_rows
        || component.quantized_blocks.len() != grid_blocks
    {
        return Err(DctWindowOutOfRange);
    }
    let stride = component.block_cols as usize;
    for row in block_y as usize..y_end as usize {
        let start = row * stride + block_x as usize;
        dst.extend_from_slice(&component.quantized_blocks[start..start + cols as usize]);
    }
    Ok(())
}

/// How a candidate DCT image's coding profile differs from a reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DctProfileMismatch {
    /// The color space or component count differs.
    ColorProfile,
    /// The component at this index has different sampling or quant tables.
    Component(usize),
}

/// Check that blocks of `candidate` can share one frame with `reference`:
/// same color space, components, sampling factors, and quant tables.
pub(crate) fn compare_dct_coding_profile(
    reference: &JpegDctImage,
    candidate: &JpegDctImage,
) -> Result<(), DctProfileMismatch> {
    if candidate.color_space != reference.color_space
        || candidate.components.len() != reference.components.len()
    {
        return Err(DctProfileMismatch::ColorProfile);
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
            return Err(DctProfileMismatch::Component(idx));
        }
    }
    Ok(())
}

/// Assemble a `width` x `height` baseline sequential DCT image from
/// per-component quantized blocks laid out on the image's MCU grid, taking
/// color space, sampling factors, and quant tables from `reference`.
pub(crate) fn assemble_baseline_dct_image(
    reference: &JpegDctImage,
    width: u32,
    height: u32,
    blocks: Vec<Vec<[i16; 64]>>,
) -> Result<JpegDctImage, WsiError> {
    let (mcu_width, mcu_height) =
        dct_mcu_dimensions(reference).ok_or_else(|| WsiError::Unsupported {
            reason: "DCT image assembly requires nonzero MCU dimensions".into(),
        })?;
    if blocks.len() != reference.components.len() {
        return Err(WsiError::Unsupported {
            reason: format!(
                "DCT image assembly got blocks for {} of {} components",
                blocks.len(),
                reference.components.len()
            ),
        });
    }
    let mcu_cols = width.div_ceil(mcu_width);
    let mcu_rows = height.div_ceil(mcu_height);
    let components = reference
        .components
        .iter()
        .zip(blocks)
        .enumerate()
        .map(|(idx, (component, quantized_blocks))| {
            let h = u32::from(component.h_samp);
            let v = u32::from(component.v_samp);
            let grid = mcu_cols.checked_mul(h).zip(mcu_rows.checked_mul(v));
            let Some((block_cols, block_rows)) = grid.filter(|(cols, rows)| {
                (*cols as usize).checked_mul(*rows as usize) == Some(quantized_blocks.len())
            }) else {
                return Err(WsiError::Unsupported {
                    reason: format!(
                        "DCT image assembly got {} blocks for component {idx}, expected a {mcu_cols}x{mcu_rows} MCU grid",
                        quantized_blocks.len()
                    ),
                });
            };
            Ok(JpegDctComponent {
                component_index: idx,
                width: width.saturating_mul(h).div_ceil(mcu_width / 8),
                height: height.saturating_mul(v).div_ceil(mcu_height / 8),
                h_samp: component.h_samp,
                v_samp: component.v_samp,
                block_cols,
                block_rows,
                quant_table: component.quant_table,
                quantized_blocks,
                dequantized_blocks: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>, WsiError>>()?;

    Ok(JpegDctImage {
        width,
        height,
        color_space: reference.color_space,
        coding_mode: JpegDctCodingMode::BaselineSequential,
        scan_count: 1,
        components,
        restart_index: None,
    })
}
