use crate::core::types::{
    ColorSpace, CpuTile, CpuTileData, CpuTileLayout, SampleType, Series, TileLayout,
};
use crate::error::WsiError;

pub(super) fn metadata_probe_coordinate(layout: &TileLayout) -> Option<(i64, i64)> {
    match layout {
        TileLayout::Regular {
            tiles_across,
            tiles_down,
            ..
        } => (*tiles_across > 0 && *tiles_down > 0).then_some((0, 0)),
        TileLayout::WholeLevel { width, height, .. } => {
            (*width > 0 && *height > 0).then_some((0, 0))
        }
        TileLayout::Irregular { tiles, .. } => tiles
            .keys()
            .min_by(|(col_a, row_a), (col_b, row_b)| row_a.cmp(row_b).then(col_a.cmp(col_b)))
            .copied(),
    }
}

pub(super) fn checked_region_pixels_usize(width: u32, height: u32) -> Result<usize, WsiError> {
    (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| WsiError::DisplayConversion("region pixel count overflow".into()))
}

pub(super) fn checked_total_samples(
    width: u32,
    height: u32,
    channels: u16,
) -> Result<usize, WsiError> {
    checked_region_pixels_usize(width, height)?
        .checked_mul(usize::from(channels))
        .ok_or_else(|| WsiError::DisplayConversion("region sample count overflow".into()))
}

fn zero_sample_data(total_samples: usize, sample_type: SampleType) -> CpuTileData {
    match sample_type {
        SampleType::Uint8 => CpuTileData::u8(vec![0u8; total_samples]),
        SampleType::Uint16 => CpuTileData::u16(vec![0u16; total_samples]),
        SampleType::Float32 => CpuTileData::f32(vec![0.0f32; total_samples]),
    }
}

pub(super) fn zero_sample_buffer_from_template(
    width: u32,
    height: u32,
    template: &CpuTile,
) -> Result<CpuTile, WsiError> {
    let total_samples = checked_total_samples(width, height, template.channels)?;
    Ok(CpuTile {
        width,
        height,
        channels: template.channels,
        color_space: template.color_space.clone(),
        layout: template.layout,
        data: zero_sample_data(total_samples, template.data.sample_type()),
    })
}

pub(super) fn zero_sample_buffer_from_series(
    width: u32,
    height: u32,
    series: &Series,
) -> Result<CpuTile, WsiError> {
    let channels = if series.channels.is_empty() {
        1u16
    } else {
        series.channels.len() as u16
    };
    let color_space = match channels {
        1 => ColorSpace::Grayscale,
        3 => ColorSpace::Rgb,
        4 => ColorSpace::Rgba,
        _ => ColorSpace::Unknown,
    };
    let total_samples = checked_total_samples(width, height, channels)?;
    Ok(CpuTile {
        width,
        height,
        channels,
        color_space,
        layout: CpuTileLayout::Interleaved,
        data: zero_sample_data(total_samples, series.sample_type),
    })
}

pub(crate) fn crop_rgb_interleaved_u8_buffer(
    src: &CpuTile,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<CpuTile, WsiError> {
    if src.layout != CpuTileLayout::Interleaved || src.channels != 3 {
        return Err(WsiError::DisplayConversion(
            "RGB crop expects 3-channel interleaved data".into(),
        ));
    }
    if x > src.width
        || y > src.height
        || x.saturating_add(width) > src.width
        || y.saturating_add(height) > src.height
    {
        return Err(WsiError::DisplayConversion(format!(
            "crop {}x{} at {},{} exceeds source {}x{}",
            width, height, x, y, src.width, src.height
        )));
    }

    let src_data = src
        .data
        .as_u8()
        .ok_or_else(|| WsiError::DisplayConversion("RGB crop expects U8 source data".into()))?;
    let src_stride = (src.width as usize)
        .checked_mul(3)
        .ok_or_else(|| WsiError::DisplayConversion("RGB crop source stride overflow".into()))?;
    let dst_stride = (width as usize).checked_mul(3).ok_or_else(|| {
        WsiError::DisplayConversion("RGB crop destination stride overflow".into())
    })?;
    let out_len = dst_stride.checked_mul(height as usize).ok_or_else(|| {
        WsiError::DisplayConversion("RGB crop destination byte count overflow".into())
    })?;
    let mut out = vec![0u8; out_len];
    for row in 0..height as usize {
        let src_start = (y as usize + row) * src_stride + x as usize * 3;
        let src_end = src_start + dst_stride;
        let dst_start = row * dst_stride;
        out[dst_start..dst_start + dst_stride].copy_from_slice(&src_data[src_start..src_end]);
    }

    Ok(CpuTile {
        width,
        height,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(out),
    })
}
