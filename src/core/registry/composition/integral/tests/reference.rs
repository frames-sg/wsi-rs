use super::*;
pub(in crate::core::registry::composition) fn compose_dense_integral_u8_rows(
    dense_hits: &[DenseIntegralU8Hit<'_>],
    shape: CompositionShape,
    total_samples: usize,
) -> Result<Option<Vec<u8>>, WsiError> {
    let out_width_i64 = shape.width as i64;
    let mut out = Vec::with_capacity(total_samples);
    for dst_y in 0..shape.height {
        let dst_y_i64 = dst_y as i64;
        let mut cursor = 0usize;
        for entry in dense_hits {
            let Some(src_bottom) = entry.hit.dest_y.checked_add(entry.height) else {
                return Err(WsiError::DisplayConversion(
                    "tile destination y overflow".into(),
                ));
            };
            if dst_y_i64 < entry.hit.dest_y || dst_y_i64 >= src_bottom {
                continue;
            }

            let dst_start_i64 = entry.hit.dest_x.max(0);
            let dst_end_i64 = entry
                .hit
                .dest_x
                .checked_add(entry.width)
                .ok_or_else(|| WsiError::DisplayConversion("tile destination x overflow".into()))?
                .min(out_width_i64);
            if dst_end_i64 <= dst_start_i64 {
                continue;
            }
            // Both coordinates are nonnegative and capped by a width that
            // originated as usize, so these conversions are lossless.
            let dst_start = dst_start_i64 as usize;
            let dst_end = dst_end_i64 as usize;
            if dst_start != cursor {
                return Ok(None);
            }

            // The row intersection checks above prove these source coordinates
            // are nonnegative and within the u32-sized decoded tile.
            let src_y = (dst_y_i64 - entry.hit.dest_y) as usize;
            let src_x = 0i64.max(-entry.hit.dest_x) as usize;
            let src_x_samples = src_x.checked_mul(shape.channels).ok_or_else(|| {
                WsiError::DisplayConversion("tile source x byte offset overflow".into())
            })?;
            let src_start = src_y
                .checked_mul(entry.row_stride)
                .and_then(|row| row.checked_add(src_x_samples))
                .ok_or_else(|| WsiError::DisplayConversion("tile source offset overflow".into()))?;
            let len = dst_end
                .checked_sub(dst_start)
                .and_then(|pixels| pixels.checked_mul(shape.channels))
                .ok_or_else(|| WsiError::DisplayConversion("tile copy length overflow".into()))?;
            let src_end = src_start
                .checked_add(len)
                .ok_or_else(|| WsiError::DisplayConversion("tile source end overflow".into()))?;
            let row = entry.data.get(src_start..src_end).ok_or_else(|| {
                WsiError::DisplayConversion("tile source row exceeds decoded buffer".into())
            })?;
            out.extend_from_slice(row);
            cursor = dst_end;
        }
        if cursor != shape.width {
            return Ok(None);
        }
    }

    if out.len() != total_samples {
        return Err(WsiError::DisplayConversion(format!(
            "dense compositor produced {} samples, expected {}",
            out.len(),
            total_samples
        )));
    }

    Ok(Some(out))
}
