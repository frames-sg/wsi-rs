//! Admit directory geometry before czi-rs computes signed bounding rectangles.

use std::io::{Read, Seek};

use super::{checked_add, preflight_error, read_exact_at, read_i32};
use crate::error::WsiError;

pub(super) fn checked_dimension_bounds(start: i32, size: i32) -> Result<(i32, i32), WsiError> {
    if size < 0 {
        return Err(preflight_error("CZI directory dimension size is negative"));
    }
    let end = start
        .checked_add(size)
        .ok_or_else(|| preflight_error("CZI directory dimension end overflows"))?;
    Ok((start, end))
}

pub(super) fn validate_entries(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    start: u64,
    payload_bytes: u64,
    entry_count: usize,
) -> Result<(), WsiError> {
    let end = checked_add(start, payload_bytes, "directory payload")?;
    let mut cursor = start;
    let mut bounds: [Option<(i32, i32)>; 2] = [None, None];
    for _ in 0..entry_count {
        let header = read_within(reader, file_len, &mut cursor, end, 32)?;
        if header.get(..2) != Some(b"DV".as_slice()) {
            return Err(preflight_error("unsupported CZI directory schema"));
        }
        let dimensions = read_i32(&header, 28, "directory dimension count")?;
        if !(0..=1024).contains(&dimensions) {
            return Err(preflight_error("invalid CZI directory dimension count"));
        }
        for _ in 0..dimensions {
            let dimension = read_within(reader, file_len, &mut cursor, end, 20)?;
            let range = checked_dimension_bounds(
                read_i32(&dimension, 4, "dimension start")?,
                read_i32(&dimension, 8, "dimension size")?,
            )?;
            let code_end = dimension[..4]
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(4);
            let code = std::str::from_utf8(&dimension[..code_end])
                .unwrap_or("")
                .trim();
            let axis = match code {
                "X" => 0,
                "Y" => 1,
                _ => continue,
            };
            let combined =
                bounds[axis].map_or(range, |prior| (prior.0.min(range.0), prior.1.max(range.1)));
            // czi-rs also stores the union's width/height in i32. Checking
            // individual ends alone does not establish that union invariant.
            combined
                .1
                .checked_sub(combined.0)
                .ok_or_else(|| preflight_error("CZI directory combined extent overflows"))?;
            bounds[axis] = Some(combined);
        }
    }
    Ok(())
}

fn read_within(
    reader: &mut (impl Read + Seek),
    file_len: u64,
    cursor: &mut u64,
    end: u64,
    length: usize,
) -> Result<Vec<u8>, WsiError> {
    let next = checked_add(*cursor, length as u64, "directory entry")?;
    if next > end {
        return Err(preflight_error("CZI directory entry exceeds its segment"));
    }
    let value = read_exact_at(reader, file_len, *cursor, length, "directory entry")?;
    *cursor = next;
    Ok(value)
}
