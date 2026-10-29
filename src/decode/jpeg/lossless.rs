//! Lossless, coefficient-domain JPEG cropping.
//!
//! A crop entropy-decodes the source to quantized DCT blocks, keeps an
//! MCU-aligned window of blocks, and entropy-codes that window as a new
//! baseline JPEG, as `jpegtran -crop` does. Quantized coefficients and quant
//! tables never change, so the crop decodes to the source's samples. Pixels of
//! unsubsampled components match exactly; subsampled chroma can differ along
//! the crop's edges, because fancy upsampling reads neighbouring blocks.
//! A single-JPEG crop whose coefficients do not fit its budget returns the
//! whole JPEG with the tile located in it, as JPEG 2000 and JPEG XR raw
//! tiles cut from a larger image do.

mod blocks;
mod source;
mod stack;

pub(crate) use blocks::{
    append_quantized_block_window, assemble_baseline_dct_image, compare_dct_coding_profile,
    dct_mcu_dimensions, DctProfileMismatch,
};

use super::dct_reemit::{encode_baseline_dct_image_with_markers, DctFrameMarkers};
use super::frame::JpegFrameInfo;
use crate::core::types::{
    Compression, EncodedTilePhotometricInterpretation, PixelRect, RawCompressedTile,
};
use crate::error::WsiError;
use source::SourceFrame;
use stack::{SourceStack, StackPart, WindowBlocks};

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
/// Peak coefficient bytes per extracted source block: quantized and
/// dequantized `i16` planes plus the progressive `i32` accumulation plane.
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

/// Upper bound on the coefficient bytes a crop of a `crop` (width, height)
/// rectangle holds when its source JPEG is at most `source` (width, height):
/// the whole source extracted, plus the cropped window.
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
/// A single-JPEG crop that needs more returns the whole JPEG instead
/// ([`lossless_crop_or_whole_jpeg`]); a stacked crop fails with
/// [`WsiError::ResourceLimit`]. Either is decided before any entropy decoding.
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

/// Crop `rect = (x, y, w, h)` out of one standalone JPEG without changing
/// quantized coefficients, or return the JPEG unchanged when the crop does
/// not fit `max_coefficient_bytes`.
///
/// The crop starts at the MCU-grid corner at or above-left of (x, y) and
/// ends at the requested far edges clamped to the source, so an aligned
/// origin yields exactly the requested tile and `tile_region` locates the
/// request otherwise. The origin must lie inside the source. Baseline and
/// progressive Huffman sources with 1 or 3 8-bit components are supported;
/// the output is baseline sequential, keeps the source's JFIF/Adobe markers,
/// component IDs and quant tables, and has no restart interval.
///
/// `max_coefficient_bytes` bounds the coefficient planes held at once: the
/// whole source's extraction plus the cropped window. The headers decide,
/// before any entropy decoding: when that estimate exceeds the budget, the
/// result is `standalone` itself, still lossless and needing no coefficient
/// memory, with `tile_region` locating the clamped request in the whole
/// image as a crop would (a caller can cut it with `jpegtran -crop`). Its
/// frame description comes from the same headers, so the photometric
/// interpretation matches the crop's. A crop larger than `max_output_bytes`,
/// which the standard Huffman tables it is coded with can make of a source
/// coded with optimized ones, also returns `standalone` when that fits.
pub(crate) fn lossless_crop_or_whole_jpeg(
    standalone: Vec<u8>,
    rect: (u32, u32, u32, u32),
    max_coefficient_bytes: u64,
    max_output_bytes: u64,
) -> Result<LosslessJpeg, WsiError> {
    let (stack, window) = parse_crop(&[standalone.as_slice()], rect)?;
    let (width, height) = (stack.width, stack.height);
    let samples_per_pixel = stack.parts[0].frame.components.len() as u16;
    if stack.coefficient_bytes(&window)? <= max_coefficient_bytes {
        let crop = crop_window(stack, &window)?;
        if crop.data.len() as u64 <= max_output_bytes || standalone.len() as u64 > max_output_bytes
        {
            return Ok(crop);
        }
    }
    let color_space = j2k_jpeg::Decoder::inspect(&standalone)
        .map_err(|err| {
            WsiError::Jpeg(format!(
                "lossless JPEG crop could not inspect the source header: {err}"
            ))
        })?
        .color_space;
    let (x, y, _, _) = rect;
    let (_, _, tile_width, tile_height) = window.tile_region;
    Ok(LosslessJpeg {
        data: standalone,
        width,
        height,
        tile_region: (x, y, tile_width, tile_height),
        info: JpegFrameInfo {
            width,
            height,
            bits_allocated: 8,
            samples_per_pixel,
            photometric_interpretation: crop_photometric(color_space)?,
        },
    })
}

/// [`lossless_crop_or_whole_jpeg`]'s crop for an image stored as standalone
/// JPEGs stacked top to bottom, such as TIFF strips. `parts[i]` covers the
/// rows starting at the sum of the previous parts' heights.
///
/// Every part must share width, components, sampling factors, quant tables
/// and color signalling, and every part but the last must be a whole number
/// of MCU rows tall. Only the parts the crop touches are entropy-decoded, one
/// at a time; `max_coefficient_bytes` bounds the largest touched part's
/// extraction plus the cropped window. No single stored JPEG holds the
/// stack, so exceeding it returns [`WsiError::ResourceLimit`] before any
/// entropy decoding.
pub(crate) fn lossless_crop_jpeg_stack(
    parts: &[&[u8]],
    rect: (u32, u32, u32, u32),
    max_coefficient_bytes: u64,
) -> Result<LosslessJpeg, WsiError> {
    let (stack, window) = parse_crop(parts, rect)?;
    let requested = stack.coefficient_bytes(&window)?;
    if requested > max_coefficient_bytes {
        return Err(WsiError::ResourceLimit {
            resource: "lossless JPEG crop DCT coefficients",
            requested,
            limit: max_coefficient_bytes,
        });
    }
    crop_window(stack, &window)
}

/// The validated headers of `parts` and the MCU window of `rect` in them.
fn parse_crop<'a>(
    parts: &[&'a [u8]],
    rect: (u32, u32, u32, u32),
) -> Result<(SourceStack<'a>, CropWindow), WsiError> {
    let (_, _, rect_width, rect_height) = rect;
    if rect_width == 0 || rect_height == 0 {
        return Err(WsiError::Unsupported {
            reason: format!(
                "lossless JPEG crop requires a nonzero rectangle, got {rect_width}x{rect_height}"
            ),
        });
    }
    let stack = SourceStack::parse(parts)?;
    let window = CropWindow::new(rect, &stack)?;
    Ok((stack, window))
}

/// Entropy-decode the parts `window` touches and code its blocks as a new
/// baseline JPEG.
fn crop_window(stack: SourceStack<'_>, window: &CropWindow) -> Result<LosslessJpeg, WsiError> {
    let WindowBlocks { profile, blocks } = stack.window_blocks(window)?;
    let image = assemble_baseline_dct_image(&profile, window.width, window.height, blocks)?;
    let reference = &stack.parts[0];
    let markers = DctFrameMarkers {
        app_segments: SourceFrame::color_app_segments(reference.data)?,
        component_ids: reference.frame.components.iter().map(|c| c.id).collect(),
    };
    let data = encode_baseline_dct_image_with_markers(&image, &markers)?;
    Ok(LosslessJpeg {
        data,
        width: window.width,
        height: window.height,
        tile_region: window.tile_region,
        info: JpegFrameInfo {
            width: window.width,
            height: window.height,
            bits_allocated: 8,
            samples_per_pixel: image.components.len() as u16,
            photometric_interpretation: crop_photometric(profile.color_space)?,
        },
    })
}

/// The photometric interpretation a crop of a `color_space` JPEG reports.
fn crop_photometric(
    color_space: j2k_jpeg::ColorSpace,
) -> Result<EncodedTilePhotometricInterpretation, WsiError> {
    match color_space {
        j2k_jpeg::ColorSpace::Grayscale => Ok(EncodedTilePhotometricInterpretation::Monochrome2),
        j2k_jpeg::ColorSpace::Rgb => Ok(EncodedTilePhotometricInterpretation::Rgb),
        j2k_jpeg::ColorSpace::YCbCr => Ok(EncodedTilePhotometricInterpretation::YbrFull422),
        other => Err(WsiError::Unsupported {
            reason: format!("lossless JPEG crop does not support {other:?} JPEG"),
        }),
    }
}

/// The cropped image in source coordinates and its MCU window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CropWindow {
    width: u32,
    height: u32,
    mcu_col: u32,
    mcu_cols: u32,
    mcu_row: u32,
    mcu_rows: u32,
    tile_region: (u32, u32, u32, u32),
}

impl CropWindow {
    fn new(rect: (u32, u32, u32, u32), stack: &SourceStack<'_>) -> Result<Self, WsiError> {
        let (x, y, w, h) = rect;
        let (src_width, src_height) = (stack.width, stack.height);
        if x >= src_width || y >= src_height {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "lossless JPEG crop origin ({x}, {y}) lies outside the {src_width}x{src_height} image"
                ),
            });
        }
        let (Some(x_end), Some(y_end)) = (x.checked_add(w), y.checked_add(h)) else {
            return Err(WsiError::Unsupported {
                reason: format!("lossless JPEG crop rectangle {w}x{h} at ({x}, {y}) overflows u32"),
            });
        };
        let (x_end, y_end) = (x_end.min(src_width), y_end.min(src_height));
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
        let part_end = part_first + part.frame.height.div_ceil(mcu_height);
        let first = self.mcu_row.max(part_first);
        let end = (self.mcu_row + self.mcu_rows).min(part_end);
        (first < end).then(|| (first - part_first, end - first))
    }
}
