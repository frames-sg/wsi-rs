//! Checked ETS headers and declared chunk-table bounds.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use byteorder::{LittleEndian, ReadBytesExt};

use crate::core::limits::{checked_product_to_usize, MAX_DECODED_IMAGE_BYTES};
use crate::core::registry::OpenBudget;
use crate::core::types::SampleType;
use crate::error::WsiError;

use super::super::invalid_slide;

pub(in crate::formats::olympus_vsi) const OLYMPUS_JPEG_2000: u32 = 3;
pub(in crate::formats::olympus_vsi) const ETS_BACKGROUND_BYTES: u64 = 40;
pub(in crate::formats::olympus_vsi) const MAX_ETS_DIMENSIONS: u32 = 16;
pub(in crate::formats::olympus_vsi) const MAX_ETS_TILES: u32 = 1_000_000;

pub(super) struct EtsHeader {
    pub(super) file_len: u64,
    pub(super) n_dimensions: u32,
    pub(super) used_chunk_offset: u64,
    pub(super) n_used_chunks: u32,
    pub(super) sample_type: SampleType,
    pub(super) samples_per_pixel: u32,
    pub(super) tile_width: u32,
    pub(super) tile_height: u32,
    pub(super) background: Vec<u8>,
    pub(super) use_pyramid: bool,
}

impl EtsHeader {
    pub(super) fn read(
        file: &mut File,
        path: &Path,
        budget: &OpenBudget,
    ) -> Result<Self, WsiError> {
        let file_len = file.metadata()?.len();
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;
        if !fourcc_matches(&magic, b"SIS") {
            return Err(invalid_slide(path, "invalid ETS SIS magic"));
        }
        let _header_size = file.read_u32::<LittleEndian>()?;
        let _version = file.read_u32::<LittleEndian>()?;
        let n_dimensions = file.read_u32::<LittleEndian>()?;
        let additional_header_offset = file.read_u64::<LittleEndian>()?;
        let _additional_header_size = file.read_u32::<LittleEndian>()?;
        file.seek(SeekFrom::Current(4))?;
        let used_chunk_offset = file.read_u64::<LittleEndian>()?;
        let n_used_chunks = file.read_u32::<LittleEndian>()?;
        file.seek(SeekFrom::Current(4))?;

        file.seek(SeekFrom::Start(additional_header_offset))?;
        file.read_exact(&mut magic)?;
        if !fourcc_matches(&magic, b"ETS") {
            return Err(invalid_slide(path, "invalid ETS header magic"));
        }
        file.seek(SeekFrom::Current(4))?;
        let pixel_type = file.read_u32::<LittleEndian>()?;
        let samples_per_pixel = file.read_u32::<LittleEndian>()?;
        let _colorspace = file.read_u32::<LittleEndian>()?;
        let compression = file.read_u32::<LittleEndian>()?;
        let _compression_quality = file.read_u32::<LittleEndian>()?;
        let tile_width = file.read_u32::<LittleEndian>()?;
        let tile_height = file.read_u32::<LittleEndian>()?;
        let _tile_z = file.read_u32::<LittleEndian>()?;
        file.seek(SeekFrom::Current(4 * 17))?;

        let sample_type = sample_type_from_ets(pixel_type)?;
        let background_len = validate_ets_header_limits(
            n_dimensions,
            n_used_chunks,
            samples_per_pixel,
            sample_type.byte_size(),
            tile_width,
            tile_height,
        )
        .map_err(|message| invalid_slide(path, message))?;
        let decoded_tile_bytes = u64::from(tile_width)
            .checked_mul(u64::from(tile_height))
            .and_then(|pixels| pixels.checked_mul(u64::from(samples_per_pixel.max(3))))
            .and_then(|samples| {
                samples.checked_mul(u64::try_from(sample_type.byte_size()).unwrap_or(u64::MAX))
            })
            .unwrap_or(u64::MAX);
        let decoded_limit = MAX_DECODED_IMAGE_BYTES.min(budget.limits().decoded_output_bytes());
        if decoded_tile_bytes > decoded_limit {
            return Err(WsiError::ResourceLimit {
                resource: "decoded output",
                requested: decoded_tile_bytes,
                limit: decoded_limit,
            });
        }
        budget.retain_metadata(u64::try_from(background_len).unwrap_or(u64::MAX))?;
        let mut background = Vec::new();
        background
            .try_reserve_exact(background_len)
            .map_err(|_| WsiError::ResourceLimit {
                resource: "Olympus ETS background metadata",
                requested: u64::try_from(background_len).unwrap_or(u64::MAX),
                limit: budget.limits().aggregate_metadata_bytes(),
            })?;
        background.resize(background_len, 0);
        file.read_exact(&mut background)?;
        let remaining_background = ETS_BACKGROUND_BYTES as usize - background_len;
        file.seek(SeekFrom::Current(remaining_background as i64))?;
        let _component_order = file.read_u32::<LittleEndian>()?;
        let use_pyramid = file.read_u32::<LittleEndian>()? != 0;

        if compression != OLYMPUS_JPEG_2000 {
            return Err(invalid_slide(
                path,
                format!("unsupported ETS compression {compression}"),
            ));
        }
        validate_ets_chunk_table(file_len, used_chunk_offset, n_dimensions, n_used_chunks)
            .map_err(|message| invalid_slide(path, message))?;

        Ok(Self {
            file_len,
            n_dimensions,
            used_chunk_offset,
            n_used_chunks,
            sample_type,
            samples_per_pixel,
            tile_width,
            tile_height,
            background,
            use_pyramid,
        })
    }
}

pub(in crate::formats::olympus_vsi) fn validate_ets_header_limits(
    n_dimensions: u32,
    n_used_chunks: u32,
    samples_per_pixel: u32,
    sample_bytes: usize,
    tile_width: u32,
    tile_height: u32,
) -> Result<usize, String> {
    if !(3..=MAX_ETS_DIMENSIONS).contains(&n_dimensions) {
        return Err(format!(
            "ETS coordinate dimensionality {n_dimensions} is outside the supported range 3..={MAX_ETS_DIMENSIONS}"
        ));
    }
    if !(1..=MAX_ETS_TILES).contains(&n_used_chunks) {
        return Err(format!(
            "ETS tile count {n_used_chunks} is outside the supported range 1..={MAX_ETS_TILES}"
        ));
    }
    if samples_per_pixel == 0 {
        return Err("ETS samples per pixel must be nonzero".into());
    }
    if tile_width == 0 || tile_height == 0 {
        return Err(format!(
            "ETS tile dimensions must be nonzero, got {tile_width}x{tile_height}"
        ));
    }
    checked_product_to_usize(
        &[
            u64::from(samples_per_pixel),
            u64::try_from(sample_bytes).unwrap_or(u64::MAX),
        ],
        ETS_BACKGROUND_BYTES,
        "Olympus ETS background",
    )
}

pub(in crate::formats::olympus_vsi) fn validate_ets_chunk_table(
    file_len: u64,
    offset: u64,
    n_dimensions: u32,
    n_used_chunks: u32,
) -> Result<(), String> {
    let entry_bytes = u64::from(n_dimensions)
        .checked_mul(4)
        .and_then(|coordinate_bytes| coordinate_bytes.checked_add(20))
        .ok_or_else(|| "ETS chunk-table entry size overflows".to_string())?;
    let table_bytes = entry_bytes
        .checked_mul(u64::from(n_used_chunks))
        .ok_or_else(|| "ETS chunk-table size overflows".to_string())?;
    let table_end = offset
        .checked_add(table_bytes)
        .ok_or_else(|| "ETS chunk-table range overflows".to_string())?;
    if table_end > file_len {
        return Err(format!(
            "ETS chunk-table range {offset}..{table_end} exceeds file length {file_len}"
        ));
    }
    Ok(())
}

pub(in crate::formats::olympus_vsi) fn sample_type_from_ets(
    pixel_type: u32,
) -> Result<SampleType, WsiError> {
    match pixel_type {
        1 | 2 => Ok(SampleType::Uint8),
        3 | 4 => Ok(SampleType::Uint16),
        9 => Ok(SampleType::Float32),
        other => Err(WsiError::UnsupportedFormat(format!(
            "unsupported ETS pixel type {other}"
        ))),
    }
}

pub(in crate::formats::olympus_vsi) fn fourcc_matches(bytes: &[u8; 4], tag: &[u8; 3]) -> bool {
    &bytes[..3] == tag && (bytes[3] == 0 || bytes[3] == b' ')
}
