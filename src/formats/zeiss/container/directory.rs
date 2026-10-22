use std::io::{Read, Seek};

use super::{format::*, model::*, pixel::*};
use crate::error::WsiError;
use crate::formats::zeiss::preflight::{
    checked_add, preflight_error as invalid, read_exact_at, read_i32, read_segment_header, read_u64,
};
use crate::formats::zeiss::{MAX_CZI_DIRECTORY_BYTES, MAX_CZI_SUBBLOCKS};

pub(super) fn parse_directory(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    offset: u64,
) -> Result<(Vec<DirectorySubBlockInfo>, SubBlockStatistics), WsiError> {
    let effective_size = read_segment_header(
        reader,
        file_len,
        offset,
        DIRECTORY_MAGIC,
        DIRECTORY_FIXED_BYTES,
        "subblock directory",
    )?;
    let fixed_offset = checked_add(offset, SEGMENT_HEADER_BYTES, "directory header")?;
    let fixed = read_exact_at(
        reader,
        file_len,
        fixed_offset,
        DIRECTORY_FIXED_BYTES as usize,
        "subblock directory header",
    )?;
    let count = read_i32(&fixed, 0, "subblock count")?;
    if count < 0 {
        return Err(invalid("CZI subblock count is negative"));
    }
    let count = usize::try_from(count).map_err(|_| invalid("CZI subblock count is invalid"))?;
    let payload_bytes = effective_size - DIRECTORY_FIXED_BYTES;
    if count > MAX_CZI_SUBBLOCKS || payload_bytes > MAX_CZI_DIRECTORY_BYTES {
        return Err(invalid("CZI subblock directory exceeds its safety limit"));
    }
    let mut cursor = checked_add(fixed_offset, DIRECTORY_FIXED_BYTES, "directory payload")?;
    let end = checked_add(cursor, payload_bytes, "directory payload")?;
    let mut subblocks = Vec::with_capacity(count);
    let mut statistics = SubBlockStatistics::default();
    for index in 0..count {
        let fixed = read_within(
            reader,
            file_len,
            &mut cursor,
            end,
            DIRECTORY_ENTRY_FIXED_BYTES,
            "directory entry",
        )?;
        if fixed.get(..2) != Some(b"DV".as_slice()) {
            return Err(invalid("unsupported CZI directory schema"));
        }
        let pixel_type_raw = read_i32(&fixed, 2, "directory pixel type")?;
        let pixel_type = PixelType::from_raw(pixel_type_raw)
            .ok_or_else(|| invalid(format!("unsupported CZI pixel type {pixel_type_raw}")))?;
        let compression_raw = read_i32(&fixed, 18, "directory compression")?;
        let compression = CompressionMode::from_raw(compression_raw)
            .ok_or_else(|| invalid(format!("unsupported CZI compression {compression_raw}")))?;
        let dimension_count = read_i32(&fixed, 28, "directory dimension count")?;
        if !(0..=1_024).contains(&dimension_count) {
            return Err(invalid("invalid CZI directory dimension count"));
        }
        let dimensions_bytes = u64::try_from(dimension_count)
            .ok()
            .and_then(|count| count.checked_mul(DIMENSION_ENTRY_BYTES))
            .ok_or_else(|| invalid("CZI directory dimension size overflows"))?;
        let dimensions = read_within(
            reader,
            file_len,
            &mut cursor,
            end,
            dimensions_bytes,
            "directory dimensions",
        )?;

        let mut coordinate = Coordinate::new();
        let mut rect = IntRect::new(0, 0, -1, -1);
        let mut stored_size = IntSize { w: 0, h: 0 };
        let mut has_x = false;
        let mut has_y = false;
        let mut m_index = None;
        for dimension in dimensions.as_chunks::<20>().0 {
            let code = parse_fixed_string(&dimension[..4]);
            let start = read_i32(dimension, 4, "dimension start")?;
            let size = read_i32(dimension, 8, "dimension size")?;
            let stored = read_i32(dimension, 16, "dimension stored size")?;
            if size < 0 || start.checked_add(size).is_none() || stored < 0 {
                return Err(invalid("invalid CZI directory dimension geometry"));
            }
            match Dimension::from_code(&code) {
                Some(Dimension::X) => {
                    rect.x = start;
                    rect.w = size;
                    stored_size.w =
                        u32::try_from(stored).map_err(|_| invalid("invalid CZI stored X size"))?;
                    has_x = true;
                }
                Some(Dimension::Y) => {
                    rect.y = start;
                    rect.h = size;
                    stored_size.h =
                        u32::try_from(stored).map_err(|_| invalid("invalid CZI stored Y size"))?;
                    has_y = true;
                }
                Some(Dimension::M) => m_index = Some(start),
                Some(dimension) if dimension.is_frame_dimension() => {
                    coordinate.set(dimension, start);
                }
                _ => {}
            }
        }
        if !has_x || !has_y {
            return Err(invalid(format!(
                "CZI directory entry {index} has no X/Y dimensions"
            )));
        }
        let info = DirectorySubBlockInfo {
            index,
            file_position: read_u64(&fixed, 6, "subblock file offset")?,
            file_part: read_i32(&fixed, 14, "subblock file part")?,
            pixel_type,
            compression,
            coordinate,
            rect,
            stored_size,
            m_index,
            pyramid_type: SubBlockPyramidType::from_raw(fixed[22]),
        };
        update_statistics(&mut statistics, &info)?;
        subblocks.push(info);
    }
    Ok((subblocks, statistics))
}

fn update_statistics(
    statistics: &mut SubBlockStatistics,
    entry: &DirectorySubBlockInfo,
) -> Result<(), WsiError> {
    statistics.subblock_count += 1;
    statistics.bounding_box = Some(match statistics.bounding_box {
        Some(rect) => rect
            .checked_union(entry.rect)
            .ok_or_else(|| invalid("CZI combined subblock extent overflows"))?,
        None => entry.rect,
    });
    if entry.is_layer0() {
        statistics.bounding_box_layer0 = Some(match statistics.bounding_box_layer0 {
            Some(rect) => rect
                .checked_union(entry.rect)
                .ok_or_else(|| invalid("CZI layer-0 extent overflows"))?,
            None => entry.rect,
        });
    }
    for (dimension, value) in entry.coordinate.iter() {
        statistics.dim_bounds.update_value(dimension, value);
    }
    if let Some(m_index) = entry.m_index {
        statistics.min_m_index = Some(
            statistics
                .min_m_index
                .map_or(m_index, |value| value.min(m_index)),
        );
        statistics.max_m_index = Some(
            statistics
                .max_m_index
                .map_or(m_index, |value| value.max(m_index)),
        );
    }
    if let Some(scene_index) = entry.coordinate.get(Dimension::S) {
        let scene = statistics
            .scene_bounding_boxes
            .entry(scene_index)
            .or_insert(BoundingBoxes {
                all: entry.rect,
                layer0: IntRect::new(0, 0, -1, -1),
            });
        scene.all = scene
            .all
            .checked_union(entry.rect)
            .ok_or_else(|| invalid("CZI scene extent overflows"))?;
        if entry.is_layer0() {
            scene.layer0 = if scene.layer0.is_valid() {
                scene
                    .layer0
                    .checked_union(entry.rect)
                    .ok_or_else(|| invalid("CZI scene layer-0 extent overflows"))?
            } else {
                entry.rect
            };
        }
    }
    Ok(())
}
