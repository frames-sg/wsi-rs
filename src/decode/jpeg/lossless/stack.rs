//! Stacked lossless-crop sources: per-part header validation, coefficient
//! budgeting from j2k-jpeg's estimates, and part-by-part windowed extraction.

use j2k_jpeg::transcode::{
    estimate_dct_windows, extract_dct_windows, DctExtractOptions, JpegDctImage, McuRect,
};
use j2k_jpeg::JpegError;

use super::blocks::{compare_dct_coding_profile, DctProfileMismatch};
use super::source::SourceFrame;
use super::CropFailure;
use crate::error::WsiError;

/// Bytes of one 8x8 block of `i16` coefficients.
const BLOCK_BYTES: u64 = 64 * 2;

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

/// A part the crop window touches.
pub(super) struct TouchedPart {
    /// Position of the part in the stack.
    pub(super) index: usize,
    /// The crop window's MCUs inside the part.
    pub(super) window: McuRect,
    /// j2k-jpeg's peak working set for extracting `window`, estimated from
    /// the part's headers.
    pub(super) peak_bytes: u64,
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
                stack.width = frame.width();
            } else {
                validate_part_matches(idx, &frame, &stack.parts[0].frame)?;
            }
            let y = stack.height;
            stack.height = y
                .checked_add(frame.height())
                .ok_or_else(|| WsiError::Unsupported {
                    reason: "lossless JPEG crop stack height overflows u32".into(),
                })?;
            stack.parts.push(StackPart { data, frame, y });
        }
        let last = stack.parts.len() - 1;
        for (idx, part) in stack.parts[..last].iter().enumerate() {
            if !part.frame.height().is_multiple_of(stack.mcu_height) {
                return Err(WsiError::Unsupported {
                    reason: format!(
                        "lossless JPEG crop stack part {idx} is {} rows tall, not a multiple of the {}-row MCU height",
                        part.frame.height(),
                        stack.mcu_height
                    ),
                });
            }
        }
        Ok(stack)
    }

    /// A codec error about part `idx`, sorted as the crop sorts it.
    fn part_error(&self, idx: usize, err: JpegError) -> WsiError {
        in_part(
            CropFailure::from_decode(err).into_error(),
            idx,
            self.parts.len(),
        )
    }

    /// The parts `window` touches, each with its MCU window and j2k-jpeg's
    /// estimate of extracting it. Only headers are read.
    pub(super) fn touched_parts(&self, window: &CropWindow) -> Result<Vec<TouchedPart>, WsiError> {
        let mut touched = Vec::new();
        for (index, part) in self.parts.iter().enumerate() {
            let Some((first_row, row_count)) = window.part_mcu_rows(part, self.mcu_height) else {
                continue;
            };
            let part_window = McuRect {
                x: window.mcu_col,
                y: first_row,
                width: window.mcu_cols,
                height: row_count,
            };
            let estimate = estimate_dct_windows(
                part.data,
                &[part_window],
                DctExtractOptions::quantized_only(),
            )
            .map_err(|err| self.part_error(index, err))?;
            touched.push(TouchedPart {
                index,
                window: part_window,
                peak_bytes: estimate.peak_bytes as u64,
            });
        }
        Ok(touched)
    }

    /// Blocks of each component in the crop window.
    fn window_block_counts<'s>(&'s self, window: &CropWindow) -> impl Iterator<Item = usize> + 's {
        let mcus = window.mcu_cols as usize * window.mcu_rows as usize;
        self.parts[0]
            .frame
            .mcu_blocks()
            .map(move |blocks| mcus * blocks)
    }

    /// Coefficient bytes held at peak: the cropped window's quantized blocks
    /// plus the largest working set of extracting one touched part.
    pub(super) fn coefficient_bytes(&self, window: &CropWindow, touched: &[TouchedPart]) -> u64 {
        let largest_part = touched.iter().map(|part| part.peak_bytes).max();
        let window_blocks: usize = self.window_block_counts(window).sum();
        (window_blocks as u64 * BLOCK_BYTES).saturating_add(largest_part.unwrap_or(0))
    }

    /// Entropy-decode the window of each touched part, one part at a time,
    /// and collect the crop window's quantized blocks per component. Parts
    /// must share the first touched part's coding profile.
    pub(super) fn window_blocks(
        &self,
        window: &CropWindow,
        touched: &[TouchedPart],
    ) -> Result<WindowBlocks, WsiError> {
        let mut blocks = self
            .window_block_counts(window)
            .map(Vec::with_capacity)
            .collect::<Vec<_>>();
        let mut profile: Option<(usize, JpegDctImage)> = None;
        for part in touched {
            let idx = part.index;
            let mut image = extract_dct_windows(
                self.parts[idx].data,
                &[part.window],
                DctExtractOptions::quantized_only(),
            )
            .map_err(|err| self.part_error(idx, err))?
            .pop()
            .ok_or_else(|| {
                WsiError::Jpeg("lossless JPEG crop extraction returned no window".into())
            })?;
            if let Some((first, profile)) = &profile {
                compare_dct_coding_profile(profile, &image)
                    .map_err(|mismatch| profile_mismatch(idx, *first, mismatch))?;
            }
            for (component, dst) in image.components.iter_mut().zip(blocks.iter_mut()) {
                // Taken, so the part's window is released once it is copied.
                dst.extend(std::mem::take(&mut component.quantized_blocks));
            }
            profile.get_or_insert((idx, image));
        }
        let (_, profile) = profile.ok_or_else(|| {
            WsiError::Jpeg("lossless JPEG crop window touches no stack part".into())
        })?;
        Ok(WindowBlocks { profile, blocks })
    }
}

fn profile_mismatch(idx: usize, first: usize, mismatch: DctProfileMismatch) -> WsiError {
    WsiError::Unsupported {
        reason: match mismatch {
            DctProfileMismatch::ColorProfile => format!(
                "lossless JPEG crop stack part {idx} decodes to a different color space than part {first}"
            ),
            DctProfileMismatch::Component(component) => format!(
                "lossless JPEG crop stack part {idx} component {component} uses a different quant table or sampling than part {first}"
            ),
        },
    }
}

/// Prefix a part's error with its stack position.
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

/// Refuses a part whose header cannot share a frame with part 0's. Quant
/// tables are matched when parts are extracted.
fn validate_part_matches(
    idx: usize,
    frame: &SourceFrame,
    first: &SourceFrame,
) -> Result<(), WsiError> {
    let (sof, first_sof) = (&frame.sof, &first.sof);
    let sampling = sof.sampling.components().iter();
    let differing = sampling
        .zip(first_sof.sampling.components())
        .enumerate()
        .find(|(_, (actual, expected))| actual != expected);
    let what = if frame.width() != first.width() {
        format!(
            "is {} pixels wide, part 0 is {}",
            frame.width(),
            first.width()
        )
    } else if sof.component_ids != first_sof.component_ids {
        format!(
            "has components {:?}, part 0 has {:?}",
            sof.component_ids, first_sof.component_ids
        )
    } else if let Some((component, (actual, expected))) = differing {
        format!(
            "component {component} samples {}x{}, part 0 samples {}x{}",
            actual.0, actual.1, expected.0, expected.1
        )
    } else if frame.color != first.color {
        "signals color with different JFIF/Adobe markers than part 0".into()
    } else {
        return Ok(());
    };
    Err(WsiError::Unsupported {
        reason: format!("lossless JPEG crop stack part {idx} {what}"),
    })
}

/// The cropped image in source coordinates and its MCU window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CropWindow {
    pub(super) width: u32,
    pub(super) height: u32,
    mcu_col: u32,
    mcu_cols: u32,
    mcu_row: u32,
    mcu_rows: u32,
    pub(super) tile_region: (u32, u32, u32, u32),
}

impl CropWindow {
    pub(super) fn new(
        rect: (u32, u32, u32, u32),
        stack: &SourceStack<'_>,
    ) -> Result<Self, WsiError> {
        let (x, y, _, _) = rect;
        let (x_end, y_end) = super::clamp_rect(rect, (stack.width, stack.height))?;
        let x0 = x - x % stack.mcu_width;
        let y0 = y - y % stack.mcu_height;
        let (width, height) = (x_end - x0, y_end - y0);
        if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "lossless JPEG crop of {width}x{height} exceeds the 65535-pixel JPEG SOF limit"
                ),
            });
        }
        Ok(Self {
            width,
            height,
            mcu_col: x0 / stack.mcu_width,
            mcu_cols: width.div_ceil(stack.mcu_width),
            mcu_row: y0 / stack.mcu_height,
            mcu_rows: height.div_ceil(stack.mcu_height),
            tile_region: (x - x0, y - y0, x_end - x, y_end - y),
        })
    }

    /// The window's MCU rows inside `part`, as (first row within the part,
    /// row count); `None` when the window does not touch the part.
    fn part_mcu_rows(&self, part: &StackPart<'_>, mcu_height: u32) -> Option<(u32, u32)> {
        let part_first = part.y / mcu_height;
        let part_end = part_first + part.frame.height().div_ceil(mcu_height);
        let first = self.mcu_row.max(part_first);
        let end = (self.mcu_row + self.mcu_rows).min(part_end);
        (first < end).then(|| (first - part_first, end - first))
    }
}
