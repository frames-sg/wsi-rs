//! Lossless, coefficient-domain JPEG cropping.
//!
//! A crop entropy-decodes an MCU-aligned window of the source to quantized
//! DCT blocks and entropy-codes that window as a new baseline JPEG, as
//! `jpegtran -crop` does. Quantized coefficients and quant tables never
//! change, so the crop decodes to the source's samples. Pixels of
//! unsubsampled components match exactly; subsampled chroma can differ along
//! the crop's edges, because fancy upsampling reads neighbouring blocks.
//!
//! j2k-jpeg does the cropping: [`j2k_jpeg::transcode::lossless_crop`] for one
//! stored JPEG, and windowed coefficient extraction for JPEGs stacked into
//! one image, which this module assembles and re-emits. What stays here is
//! WSI policy: a single-JPEG crop that is not available returns the whole
//! JPEG with the tile located in it, as JPEG 2000 and JPEG XR raw tiles cut
//! from a larger image do.

mod blocks;
mod source;
mod stack;

pub(crate) use blocks::{
    assemble_baseline_dct_image, compare_dct_coding_profile, dct_mcu_dimensions, DctProfileMismatch,
};

use j2k_jpeg::transcode::{lossless_crop, LosslessCropError, LosslessCropOptions};
use j2k_jpeg::{JpegEncodeError, JpegError, SofKind};

use super::dct_reemit::{encode_baseline_dct_image_with_markers, DctFrameMarkers};
use super::frame::JpegFrameInfo;
use crate::core::types::{
    Compression, EncodedTilePhotometricInterpretation, PixelRect, RawCompressedTile,
};
use crate::error::WsiError;
use source::SourceFrame;
use stack::{CropWindow, SourceStack, TouchedPart, WindowBlocks};

/// The resources a crop's coefficient working set and output are charged to.
const COEFFICIENT_RESOURCE: &str = "lossless JPEG crop DCT coefficients";
const OUTPUT_RESOURCE: &str = "lossless JPEG crop output";
/// JPEG's limit on the 8x8 blocks of one MCU (T.81 B.2.3).
const MAX_BLOCKS_PER_MCU: u32 = 10;

/// Result of a lossless JPEG crop: the cropped window, or the whole source
/// when the crop does not fit its limits.
#[derive(Debug, Clone)]
pub(crate) struct LosslessJpeg {
    /// Standalone JPEG: a baseline crop, or the source unchanged, which may
    /// be progressive.
    pub(crate) data: Vec<u8>,
    /// Dimensions encoded in its SOF.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The requested rectangle inside `data`'s image: (x, y, width, height).
    /// x/y are non-zero when a crop starts at an earlier MCU boundary, or
    /// when the whole source holds the request away from its origin.
    pub(crate) tile_region: (u32, u32, u32, u32),
    /// Bits, samples, and photometric interpretation of `data`.
    pub(crate) info: JpegFrameInfo,
}

impl LosslessJpeg {
    /// A raw tile of this result that must cover a `tile` of (width, height).
    ///
    /// Color signalling is read back the way whole-image passthrough reads a
    /// stored JPEG, so a crop and its source report the same photometric
    /// interpretation. `tile_region` is set unless the image is exactly the
    /// tile.
    pub(crate) fn into_raw_tile(self, tile: (u32, u32)) -> Result<RawCompressedTile, WsiError> {
        let (x, y, width, height) = self.tile_region;
        if (width, height) != tile {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "JPEG crop covers {width}x{height}, not the {}x{} tile; the stored JPEG is smaller than its container declares",
                    tile.0, tile.1
                ),
            });
        }
        let whole = (x, y, width, height) == (0, 0, self.width, self.height);
        let raw = super::standalone_raw_jpeg_tile(self.data)?;
        let builder = RawCompressedTile::builder(Compression::Jpeg)
            .dimensions(raw.width(), raw.height())
            .bits_allocated(raw.bits_allocated())
            .samples_per_pixel(raw.samples_per_pixel())
            .photometric_interpretation(raw.photometric_interpretation());
        let builder = if whole {
            builder
        } else {
            builder.tile_region(PixelRect::new(x, y, width, height))
        };
        Ok(builder.data(raw.into_data()).build()?)
    }
}

/// Worst-case entropy-coded bytes of one 8x8 block under the standard
/// Huffman tables a crop writes: an 11-bit DC difference and 63 10-bit AC
/// coefficients, each behind a code of up to 16 bits (1660 bits), doubled
/// for 0xFF byte stuffing.
const MAX_CROP_BLOCK_BYTES: u64 = 416;
/// Quantization and Huffman tables, frame and scan headers of a crop.
const CROP_HEADER_BYTES: u64 = 1024;
/// Reserved coefficient bytes per source block. j2k-jpeg extracts one
/// quantized `i16` plane (128 bytes a block) and, for progressive and
/// multi-scan sources, accumulates a whole-image `i32` plane (256 more);
/// the rest covers its restart index and result metadata.
const MAX_EXTRACTED_BLOCK_BYTES: u64 = 2 * 64 * 2 + 64 * 4;
/// Coefficient bytes per block of the cropped window.
const WINDOW_BLOCK_BYTES: u64 = 64 * 2;

/// Upper bound on the 8x8 blocks covering `width` x `height` pixels for any
/// accepted sampling: at most three blocks per 64 pixels, with up to one
/// 32-pixel MCU of origin snap and one of edge padding on each axis.
fn max_blocks(width: u32, height: u32) -> u64 {
    let side = |pixels: u32| (u64::from(pixels) + 62).div_ceil(8);
    side(width).saturating_mul(side(height)).saturating_mul(3)
}

/// Worst-case size of a lossless crop of a `width` x `height` rectangle
/// from a `source_len`-byte JPEG. The crop copies the source's JFIF and
/// Adobe segments, which the source length bounds.
pub(crate) fn lossless_crop_output_bound(source_len: u64, width: u32, height: u32) -> u64 {
    source_len
        .saturating_add(CROP_HEADER_BYTES)
        .saturating_add(max_blocks(width, height).saturating_mul(MAX_CROP_BLOCK_BYTES))
}

/// Upper bound on the coefficient working set of a crop of a `crop` (width,
/// height) rectangle when its source JPEG is at most `source` (width,
/// height): the whole source extracted, plus the cropped window. Admission
/// reserves it before the source is read; it is never below the peak
/// j2k-jpeg estimates from the source's headers
/// ([`j2k_jpeg::transcode::estimate_dct_windows`]), which gates the crop.
pub(crate) fn lossless_crop_coefficient_bound(source: (u32, u32), crop: (u32, u32)) -> u64 {
    max_blocks(source.0, source.1)
        .saturating_mul(MAX_EXTRACTED_BLOCK_BYTES)
        .saturating_add(max_blocks(crop.0, crop.1).saturating_mul(WINDOW_BLOCK_BYTES))
}

/// Coefficient budget of a crop of a `crop` (width, height) rectangle whose
/// working set may hold `reservation` bytes: what remains once the
/// `source_len` source bytes held, the JFIF/Adobe segments the crop copies
/// out of them (at most `source_len` more) and the worst-case output are set
/// aside.
/// A single-JPEG crop whose extraction j2k-jpeg estimates above it returns
/// the whole JPEG instead ([`lossless_crop_or_whole_jpeg`]); a stacked crop
/// fails with [`WsiError::ResourceLimit`]. Either is decided before any
/// entropy decoding.
pub(crate) fn lossless_crop_coefficient_budget(
    reservation: u64,
    source_len: u64,
    crop: (u32, u32),
) -> u64 {
    reservation.saturating_sub(
        source_len
            .saturating_mul(2)
            .saturating_add(lossless_crop_output_bound(source_len, crop.0, crop.1)),
    )
}

/// The raw tile `rect` of the standalone JPEG `data`: a lossless crop, or
/// `data` whole with `tile_region` locating `rect` when the crop does not
/// fit. `working_set` is what admission reserved for `data`, the
/// coefficients and the output together; `max_output_bytes` bounds the
/// crop, as in [`lossless_crop_or_whole_jpeg`].
pub(crate) fn lossless_crop_raw_tile(
    data: Vec<u8>,
    rect: PixelRect,
    working_set: u64,
    max_output_bytes: u64,
) -> Result<RawCompressedTile, WsiError> {
    let tile = (rect.width, rect.height);
    let budget = lossless_crop_coefficient_budget(working_set, data.len() as u64, tile);
    lossless_crop_or_whole_jpeg(
        data,
        (rect.x, rect.y, rect.width, rect.height),
        budget,
        max_output_bytes,
    )?
    .into_raw_tile(tile)
}

/// A crop j2k-jpeg did not make. Every codec error is sorted by variant.
#[derive(Debug)]
pub(super) enum CropFailure {
    /// The stream is intact as far as it was read, but its crop is not
    /// available: it needs more working storage than admitted, or j2k-jpeg
    /// does not cut its coding. A single stored JPEG that raw passthrough
    /// can describe is returned whole instead; otherwise this error stands.
    Unavailable(WsiError),
    /// The request or the stream is invalid. Nothing stands in for the crop.
    Invalid(WsiError),
}

impl CropFailure {
    /// The error of a crop that has no whole JPEG to return instead.
    pub(super) fn into_error(self) -> WsiError {
        match self {
            Self::Unavailable(err) | Self::Invalid(err) => err,
        }
    }

    /// Sorts a header-parse or coefficient-extraction error.
    pub(super) fn from_decode(err: JpegError) -> Self {
        match err {
            // Working storage beyond what was admitted, which j2k-jpeg
            // decides from the headers, before any entropy decoding.
            JpegError::MemoryCapExceeded { requested, cap } => {
                Self::Unavailable(WsiError::ResourceLimit {
                    resource: COEFFICIENT_RESOURCE,
                    requested: requested as u64,
                    limit: cap as u64,
                })
            }
            // Valid JPEG the crop does not cut.
            JpegError::NotImplemented { .. }
            | JpegError::UnsupportedSof { .. }
            | JpegError::UnsupportedPredictor { .. }
            | JpegError::UnsupportedBitDepth { .. }
            | JpegError::UnsupportedComponentCount { .. }
            | JpegError::UnsupportedColorSpace { .. }
            | JpegError::ZeroDimension { .. }
            | JpegError::DimensionOverflow { .. } => Self::Unavailable(WsiError::Unsupported {
                reason: format!("lossless JPEG crop does not cut this JPEG: {err}"),
            }),
            JpegError::RectOutOfBounds {
                rect,
                width,
                height,
            } => Self::Invalid(
                clamp_rect((rect.x, rect.y, rect.w, rect.h), (width, height))
                    .err()
                    .unwrap_or_else(|| WsiError::Unsupported {
                        reason: format!("lossless JPEG crop rectangle is invalid: {err}"),
                    }),
            ),
            // Everything else is a malformed or corrupt stream, or a codec
            // failure. `JpegError` is non-exhaustive: a variant added later
            // is an error too, never a silent whole-JPEG answer.
            err => Self::Invalid(WsiError::Jpeg(format!(
                "lossless JPEG crop could not read its source: {err}"
            ))),
        }
    }
}

impl From<LosslessCropError> for CropFailure {
    fn from(err: LosslessCropError) -> Self {
        match err {
            LosslessCropError::Decode(err) => Self::from_decode(err),
            // The coefficients are valid but baseline JPEG cannot hold them:
            // 16-bit quantization values, a lone component sampled 2x2, or
            // magnitudes beyond the baseline Huffman categories.
            LosslessCropError::Encode(JpegEncodeError::InvalidDctImage { reason }) => {
                Self::Unavailable(WsiError::Unsupported {
                    reason: format!(
                        "lossless JPEG crop cannot re-emit its source as baseline JPEG: {reason}"
                    ),
                })
            }
            // The crop is larger than j2k-jpeg emits.
            LosslessCropError::Encode(JpegEncodeError::MemoryCapExceeded { requested, cap }) => {
                Self::Unavailable(WsiError::ResourceLimit {
                    resource: OUTPUT_RESOURCE,
                    requested: requested as u64,
                    limit: cap as u64,
                })
            }
            LosslessCropError::Encode(err) => Self::Invalid(WsiError::Jpeg(format!(
                "lossless JPEG crop re-emission failed: {err}"
            ))),
        }
    }
}

/// Refuses a multi-component frame whose `sampling` factors (h, v) put more
/// blocks in an MCU than JPEG allows. j2k-jpeg parses such a header, so a
/// stream that breaks the standard this way would otherwise be handed on
/// whole when its crop is unavailable.
fn check_blocks_per_mcu(sampling: &[(u8, u8)]) -> Result<(), WsiError> {
    let blocks: u32 = sampling
        .iter()
        .map(|&(h, v)| u32::from(h) * u32::from(v))
        .sum();
    if sampling.len() > 1 && blocks > MAX_BLOCKS_PER_MCU {
        return Err(WsiError::Jpeg(format!(
            "lossless JPEG crop requires at most {MAX_BLOCKS_PER_MCU} blocks per MCU, got {blocks}"
        )));
    }
    Ok(())
}

/// Crop `rect = (x, y, w, h)` out of one standalone JPEG without changing
/// quantized coefficients, or return the JPEG unchanged when the crop is not
/// available.
///
/// The crop is [`j2k_jpeg::transcode::lossless_crop`]'s: it starts at the
/// MCU-grid corner at or above-left of (x, y) and ends at the requested far
/// edges clamped to the source, so an aligned origin yields exactly the
/// requested tile and `tile_region` locates the request otherwise. Baseline
/// and progressive Huffman sources with 1 or 3 8-bit components are
/// supported; the output is baseline sequential, keeps the source's
/// JFIF/Adobe markers, component IDs and quant tables, and has no restart
/// interval.
///
/// `max_coefficient_bytes` bounds the extraction's working set as j2k-jpeg
/// estimates it from the headers: the window's quantized blocks for a
/// single-scan baseline source, the whole image's accumulation for a
/// progressive or multi-scan one. When the estimate exceeds the budget,
/// nothing is entropy-decoded and the result is `standalone` itself, with
/// `tile_region` locating the clamped request in the whole image as a crop
/// would. A crop larger than `max_output_bytes`, which the standard Huffman
/// tables it is coded with can make of a source coded with optimized ones,
/// also returns `standalone` when that fits. So does an 8-bit gray or
/// 3-component baseline or progressive JPEG the crop does not cut, such as a
/// single component sampled 2x2. Other coding processes stay unsupported,
/// and a malformed or corrupt stream is always an error ([`CropFailure`]).
pub(crate) fn lossless_crop_or_whole_jpeg(
    standalone: Vec<u8>,
    rect: (u32, u32, u32, u32),
    max_coefficient_bytes: u64,
    max_output_bytes: u64,
) -> Result<LosslessJpeg, WsiError> {
    check_nonzero_rect(rect)?;
    let (x, y, w, h) = rect;
    let mut options = LosslessCropOptions::default();
    options.max_working_bytes = usize::try_from(max_coefficient_bytes).unwrap_or(usize::MAX);
    let unavailable = match lossless_crop(&standalone, j2k_jpeg::Rect { x, y, w, h }, options) {
        Ok(crop)
            if crop.data.len() as u64 <= max_output_bytes
                || standalone.len() as u64 > max_output_bytes =>
        {
            let source = whole_jpeg_info(&standalone)?.ok_or_else(|| {
                WsiError::Jpeg("lossless JPEG crop could not describe its source".into())
            })?;
            return Ok(LosslessJpeg {
                data: crop.data,
                width: crop.width,
                height: crop.height,
                tile_region: (crop.region.x, crop.region.y, crop.region.w, crop.region.h),
                info: JpegFrameInfo {
                    width: crop.width,
                    height: crop.height,
                    ..source
                },
            });
        }
        Ok(crop) => WsiError::ResourceLimit {
            resource: OUTPUT_RESOURCE,
            requested: crop.data.len() as u64,
            limit: max_output_bytes,
        },
        Err(err) => match CropFailure::from(err) {
            CropFailure::Unavailable(err) => err,
            CropFailure::Invalid(err) => return Err(err),
        },
    };
    let info = whole_jpeg_info(&standalone)?.ok_or(unavailable)?;
    let (x_end, y_end) = clamp_rect(rect, (info.width, info.height))?;
    Ok(LosslessJpeg {
        data: standalone,
        width: info.width,
        height: info.height,
        tile_region: (x, y, x_end - x, y_end - y),
        info,
    })
}

/// `standalone`'s frame from its headers, when it is an 8-bit gray or
/// 3-component baseline or progressive Huffman JPEG, as raw JPEG passthrough
/// describes them. A header j2k-jpeg parses but that breaks the JPEG
/// standard is an error: such a stream is never handed on whole.
fn whole_jpeg_info(standalone: &[u8]) -> Result<Option<JpegFrameInfo>, WsiError> {
    let Ok(header) = j2k_jpeg::Decoder::inspect(standalone) else {
        return Ok(None);
    };
    let photometric_interpretation = match header.color_space {
        j2k_jpeg::ColorSpace::Grayscale => EncodedTilePhotometricInterpretation::Monochrome2,
        j2k_jpeg::ColorSpace::Rgb => EncodedTilePhotometricInterpretation::Rgb,
        j2k_jpeg::ColorSpace::YCbCr => EncodedTilePhotometricInterpretation::YbrFull422,
        _ => return Ok(None),
    };
    let (width, height) = header.dimensions;
    if !matches!(header.sof_kind, SofKind::Baseline8 | SofKind::Progressive8)
        || !matches!(header.sampling.len(), 1 | 3)
        || width == 0
        || height == 0
    {
        return Ok(None);
    }
    check_blocks_per_mcu(header.sampling.components())?;
    Ok(Some(JpegFrameInfo {
        width,
        height,
        bits_allocated: 8,
        samples_per_pixel: header.sampling.len() as u16,
        photometric_interpretation,
    }))
}

/// [`lossless_crop_or_whole_jpeg`]'s crop for an image stored as standalone
/// JPEGs stacked top to bottom, such as TIFF strips. `parts[i]` covers the
/// rows starting at the sum of the previous parts' heights.
///
/// j2k-jpeg crops one JPEG, so the stack is assembled here from its windowed
/// extraction of each part. Every part must share width, components,
/// sampling factors and color signalling, every part the crop touches the
/// same quant tables, and every part but the last must be a whole number of
/// MCU rows tall. Only the MCU windows of the touched parts are
/// entropy-decoded, one part at a time; `max_coefficient_bytes` bounds the
/// cropped window's blocks plus the largest working set j2k-jpeg estimates
/// for one touched part. No single stored JPEG holds the stack, so exceeding
/// it returns [`WsiError::ResourceLimit`] before any entropy decoding.
pub(crate) fn lossless_crop_jpeg_stack(
    parts: &[&[u8]],
    rect: (u32, u32, u32, u32),
    max_coefficient_bytes: u64,
) -> Result<LosslessJpeg, WsiError> {
    check_nonzero_rect(rect)?;
    let stack = SourceStack::parse(parts)?;
    let window = CropWindow::new(rect, &stack)?;
    let touched = stack.touched_parts(&window)?;
    let requested = stack.coefficient_bytes(&window, &touched);
    if requested > max_coefficient_bytes {
        return Err(WsiError::ResourceLimit {
            resource: COEFFICIENT_RESOURCE,
            requested,
            limit: max_coefficient_bytes,
        });
    }
    crop_window(&stack, &window, &touched)
}

fn check_nonzero_rect((_, _, width, height): (u32, u32, u32, u32)) -> Result<(), WsiError> {
    if width == 0 || height == 0 {
        return Err(WsiError::Unsupported {
            reason: format!(
                "lossless JPEG crop requires a nonzero rectangle, got {width}x{height}"
            ),
        });
    }
    Ok(())
}

/// The far edges (x_end, y_end) of `rect` clamped to a `width` x `height`
/// image. Its origin must lie inside the image.
fn clamp_rect(
    (x, y, w, h): (u32, u32, u32, u32),
    (width, height): (u32, u32),
) -> Result<(u32, u32), WsiError> {
    if x >= width || y >= height {
        return Err(WsiError::Unsupported {
            reason: format!(
                "lossless JPEG crop origin ({x}, {y}) lies outside the {width}x{height} image"
            ),
        });
    }
    let (Some(x_end), Some(y_end)) = (x.checked_add(w), y.checked_add(h)) else {
        return Err(WsiError::Unsupported {
            reason: format!("lossless JPEG crop rectangle {w}x{h} at ({x}, {y}) overflows u32"),
        });
    };
    Ok((x_end.min(width), y_end.min(height)))
}

/// Entropy-decode `window` in the parts it touches and code its blocks as a
/// new baseline JPEG.
fn crop_window(
    stack: &SourceStack<'_>,
    window: &CropWindow,
    touched: &[TouchedPart],
) -> Result<LosslessJpeg, WsiError> {
    let WindowBlocks { profile, blocks } = stack.window_blocks(window, touched)?;
    let image = assemble_baseline_dct_image(&profile, window.width, window.height, blocks)?;
    let reference = &stack.parts[0];
    let markers = DctFrameMarkers {
        app_segments: SourceFrame::color_app_segments(reference.data)?,
        component_ids: reference.frame.sof.component_ids.clone(),
    };
    let data = encode_baseline_dct_image_with_markers(&image, &markers)?;
    let info = whole_jpeg_info(&data)?
        .ok_or_else(|| WsiError::Jpeg("lossless JPEG crop could not describe its output".into()))?;
    Ok(LosslessJpeg {
        data,
        width: window.width,
        height: window.height,
        tile_region: window.tile_region,
        info,
    })
}
