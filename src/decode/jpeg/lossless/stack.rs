//! Stacked lossless-crop sources: per-part header validation, coefficient
//! budgeting, and part-by-part block extraction.

use j2k_jpeg::transcode::{extract_dct_blocks, DctExtractOptions, JpegDctImage};

use super::blocks::{
    append_quantized_block_window, compare_dct_coding_profile, DctProfileMismatch,
    DctWindowOutOfRange,
};
use super::source::SourceFrame;
use super::CropWindow;
use crate::error::WsiError;

/// Bytes of one 8x8 block of `i16` coefficients.
const BLOCK_BYTES: u64 = 64 * 2;

/// Keep an extracted image's coding profile, releasing its coefficients.
fn without_coefficients(mut image: JpegDctImage) -> JpegDctImage {
    for component in &mut image.components {
        component.quantized_blocks = Vec::new();
        component.dequantized_blocks = Vec::new();
    }
    image.restart_index = None;
    image
}

/// The cropped window's quantized blocks, gathered from the stack.
pub(super) struct WindowBlocks {
    /// The first touched part's coding profile, without its coefficients.
    pub(super) profile: JpegDctImage,
    /// Per-component blocks laid out on the window's MCU grid.
    pub(super) blocks: Vec<Vec<[i16; 64]>>,
}

pub(super) struct StackPart<'a> {
    pub(super) data: &'a [u8],
    pub(super) frame: SourceFrame,
    /// First image row the part covers.
    pub(super) y: u32,
}

/// Validated headers of the stacked parts.
pub(super) struct SourceStack<'a> {
    pub(super) parts: Vec<StackPart<'a>>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) mcu_width: u32,
    pub(super) mcu_height: u32,
}

impl<'a> SourceStack<'a> {
    pub(super) fn parse(parts: &[&'a [u8]]) -> Result<Self, WsiError> {
        if parts.is_empty() {
            return Err(WsiError::Unsupported {
                reason: "lossless JPEG crop needs at least one JPEG".into(),
            });
        }
        let mut stack = Self {
            parts: Vec::with_capacity(parts.len()),
            width: 0,
            height: 0,
            mcu_width: 0,
            mcu_height: 0,
        };
        for (idx, data) in parts.iter().enumerate() {
            let frame = SourceFrame::parse(data).map_err(|err| in_part(err, idx, parts.len()))?;
            if idx == 0 {
                (stack.mcu_width, stack.mcu_height) = frame.mcu_dimensions();
                stack.width = frame.width;
            } else {
                validate_part_matches(idx, &frame, &stack.parts[0].frame)?;
            }
            let y = stack.height;
            stack.height = y
                .checked_add(frame.height)
                .ok_or_else(|| WsiError::Unsupported {
                    reason: "lossless JPEG crop stack height overflows u32".into(),
                })?;
            stack.parts.push(StackPart { data, frame, y });
        }
        let last = stack.parts.len() - 1;
        for (idx, part) in stack.parts[..last].iter().enumerate() {
            if !part.frame.height.is_multiple_of(stack.mcu_height) {
                return Err(WsiError::Unsupported {
                    reason: format!(
                        "lossless JPEG crop stack part {idx} is {} rows tall, not a multiple of the {}-row MCU height",
                        part.frame.height, stack.mcu_height
                    ),
                });
            }
        }
        Ok(stack)
    }

    fn part_label(&self, idx: usize) -> String {
        if self.parts.len() > 1 {
            format!(" in stack part {idx}")
        } else {
            String::new()
        }
    }

    /// Entropy-decode each part the window touches, one at a time, and
    /// collect the window's quantized blocks per component.
    pub(super) fn window_blocks(&self, window: &CropWindow) -> Result<WindowBlocks, WsiError> {
        let mut blocks = self.parts[0]
            .frame
            .components
            .iter()
            .map(|component| {
                Vec::with_capacity(
                    window.mcu_cols as usize
                        * window.mcu_rows as usize
                        * usize::from(component.h_samp)
                        * usize::from(component.v_samp),
                )
            })
            .collect::<Vec<_>>();
        let mut profile: Option<JpegDctImage> = None;
        for (idx, part) in self.parts.iter().enumerate() {
            let Some(rows) = window.part_mcu_rows(part, self.mcu_height) else {
                continue;
            };
            let image =
                extract_dct_blocks(part.data, DctExtractOptions::default()).map_err(|err| {
                    WsiError::Jpeg(format!(
                        "lossless JPEG crop DCT extraction failed{}: {err}",
                        self.part_label(idx)
                    ))
                })?;
            self.validate_extracted(idx, &image, profile.as_ref())?;
            self.append_part_window(idx, &image, window, rows, &mut blocks)?;
            if profile.is_none() {
                profile = Some(without_coefficients(image));
            }
        }
        let profile = profile.ok_or_else(|| {
            WsiError::Jpeg("lossless JPEG crop window touches no stack part".into())
        })?;
        Ok(WindowBlocks { profile, blocks })
    }

    /// Append the window's MCU columns of `rows = (first, count)` part MCU
    /// rows to each component's blocks.
    fn append_part_window(
        &self,
        idx: usize,
        image: &JpegDctImage,
        window: &CropWindow,
        (first_row, row_count): (u32, u32),
        blocks: &mut [Vec<[i16; 64]>],
    ) -> Result<(), WsiError> {
        for (component_idx, (component, dst)) in
            image.components.iter().zip(blocks.iter_mut()).enumerate()
        {
            let h = u32::from(component.h_samp);
            let v = u32::from(component.v_samp);
            let appended = match (
                window.mcu_col.checked_mul(h),
                first_row.checked_mul(v),
                window.mcu_cols.checked_mul(h),
                row_count.checked_mul(v),
            ) {
                (Some(block_x), Some(block_y), Some(cols), Some(rows)) => {
                    append_quantized_block_window(component, block_x, block_y, cols, rows, dst)
                }
                _ => Err(DctWindowOutOfRange),
            };
            if appended.is_err() {
                return Err(WsiError::Jpeg(format!(
                    "lossless JPEG crop window exceeds the block grid of component {component_idx}{}",
                    self.part_label(idx)
                )));
            }
        }
        Ok(())
    }

    /// Coefficient bytes held at peak: the largest touched part's
    /// extraction plus the cropped window's quantized blocks.
    pub(super) fn coefficient_bytes(&self, window: &CropWindow) -> Result<u64, WsiError> {
        let overflow = || WsiError::Unsupported {
            reason: "lossless JPEG crop coefficient size overflows u64".into(),
        };
        let mut largest_part = 0u64;
        for part in &self.parts {
            if window.part_mcu_rows(part, self.mcu_height).is_some() {
                largest_part =
                    largest_part.max(part.frame.extraction_bytes().ok_or_else(overflow)?);
            }
        }
        let window_bytes = u64::from(window.mcu_cols)
            .checked_mul(u64::from(window.mcu_rows))
            .and_then(|mcus| mcus.checked_mul(u64::from(self.parts[0].frame.blocks_per_mcu())))
            .and_then(|blocks| blocks.checked_mul(BLOCK_BYTES))
            .ok_or_else(overflow)?;
        largest_part.checked_add(window_bytes).ok_or_else(overflow)
    }

    /// Check an extracted part against its header and the first touched part.
    fn validate_extracted(
        &self,
        idx: usize,
        image: &JpegDctImage,
        profile: Option<&JpegDctImage>,
    ) -> Result<(), WsiError> {
        let frame = &self.parts[idx].frame;
        let label = self.part_label(idx);
        if (image.width, image.height) != (frame.width, frame.height)
            || image.components.len() != frame.components.len()
        {
            return Err(WsiError::Jpeg(format!(
                "lossless JPEG crop extraction disagrees with the SOF header{label}"
            )));
        }
        let (mcu_width, mcu_height) = frame.mcu_dimensions();
        let mcu_cols = frame.width.div_ceil(mcu_width);
        let mcu_rows = frame.height.div_ceil(mcu_height);
        for (component_idx, (component, header)) in
            image.components.iter().zip(&frame.components).enumerate()
        {
            let h = u32::from(header.h_samp);
            let v = u32::from(header.v_samp);
            if (component.h_samp, component.v_samp) != (header.h_samp, header.v_samp)
                || component.block_cols != mcu_cols * h
                || component.block_rows != mcu_rows * v
            {
                return Err(WsiError::Jpeg(format!(
                    "lossless JPEG crop component {component_idx} block grid disagrees with the SOF header{label}"
                )));
            }
        }
        let Some(profile) = profile else {
            return Ok(());
        };
        compare_dct_coding_profile(profile, image).map_err(|mismatch| WsiError::Unsupported {
            reason: match mismatch {
                DctProfileMismatch::ColorProfile => format!(
                    "lossless JPEG crop stack part {idx} decodes to a different color space than the first part"
                ),
                DctProfileMismatch::Component(component_idx) => format!(
                    "lossless JPEG crop stack part {idx} component {component_idx} uses different sampling or quant tables than the first part"
                ),
            },
        })
    }
}

/// Prefix a part's parse error with its stack position.
fn in_part(err: WsiError, idx: usize, count: usize) -> WsiError {
    if count <= 1 {
        return err;
    }
    match err {
        WsiError::Unsupported { reason } => WsiError::Unsupported {
            reason: format!("stack part {idx}: {reason}"),
        },
        WsiError::Jpeg(reason) => WsiError::Jpeg(format!("stack part {idx}: {reason}")),
        other => other,
    }
}

fn validate_part_matches(
    idx: usize,
    frame: &SourceFrame,
    first: &SourceFrame,
) -> Result<(), WsiError> {
    let mismatch = |what: String| {
        Err(WsiError::Unsupported {
            reason: format!("lossless JPEG crop stack part {idx} {what}"),
        })
    };
    if frame.width != first.width {
        return mismatch(format!(
            "is {} pixels wide, part 0 is {}",
            frame.width, first.width
        ));
    }
    if frame.components.len() != first.components.len() {
        return mismatch(format!(
            "has {} components, part 0 has {}",
            frame.components.len(),
            first.components.len()
        ));
    }
    for (component_idx, (actual, expected)) in
        frame.components.iter().zip(&first.components).enumerate()
    {
        if (actual.h_samp, actual.v_samp) != (expected.h_samp, expected.v_samp) {
            return mismatch(format!(
                "component {component_idx} samples {}x{}, part 0 samples {}x{}",
                actual.h_samp, actual.v_samp, expected.h_samp, expected.v_samp
            ));
        }
        if actual.quant_table != expected.quant_table {
            return mismatch(format!(
                "component {component_idx} uses a different quant table than part 0"
            ));
        }
        if actual.id != expected.id {
            return mismatch(format!(
                "component {component_idx} has ID {}, part 0 has ID {}",
                actual.id, expected.id
            ));
        }
    }
    if frame.color != first.color {
        return mismatch("signals color with different JFIF/Adobe markers than part 0".into());
    }
    Ok(())
}
