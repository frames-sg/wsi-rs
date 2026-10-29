// SPDX-License-Identifier: MIT OR Apache-2.0

use pyo3::{exceptions::PyValueError, prelude::*, types::PyBytes};
use wsi_rs::{
    Compression, EncodedTilePhotometricInterpretation as Photometric, RawCompressedTile,
    SlideLimits, TileLayout,
};

#[pyclass(module = "wsi_rs", frozen, get_all)]
pub(super) struct TileHit {
    pub(super) coords: (i64, i64),
    pub(super) offset: (f64, f64),
}

#[pyclass(module = "wsi_rs", frozen, get_all)]
pub(super) struct CompressedTile {
    data: Py<PyBytes>,
    coords: (i64, i64),
    width: u32,
    height: u32,
    codec: String,
    bits_allocated: u16,
    significant_bits: Option<u16>,
    samples_per_pixel: u16,
    photometric_interpretation: &'static str,
    color_transform: &'static str,
    tile_region: (u32, u32, u32, u32),
}

impl CompressedTile {
    pub(super) fn new(
        py: Python<'_>,
        tile: RawCompressedTile,
        coords: (i64, i64),
    ) -> PyResult<Self> {
        let color_transform = match (tile.compression(), tile.photometric_interpretation()) {
            (Compression::Jpeg, Photometric::Rgb) => "none",
            (Compression::Jpeg, Photometric::YbrFull422) => "ycbcr",
            _ => "auto",
        };
        let region = tile.tile_region();
        Ok(Self {
            data: PyBytes::new(py, tile.data()).unbind(),
            coords,
            width: tile.width(),
            height: tile.height(),
            codec: codec_name(tile.compression()),
            bits_allocated: tile.bits_allocated(),
            significant_bits: tile.significant_bits(),
            samples_per_pixel: tile.samples_per_pixel(),
            photometric_interpretation: photometric_name(tile.photometric_interpretation()),
            color_transform,
            tile_region: (region.x, region.y, region.width, region.height),
        })
    }
}

/// Lower-case codec name shared by compressed tiles and tile locations.
pub(super) fn codec_name(compression: Compression) -> String {
    match compression {
        Compression::Jpeg => "jpeg".into(),
        Compression::Jp2kRgb | Compression::Jp2kYcbcr => "jpeg2000".into(),
        other => format!("{other:?}").to_lowercase(),
    }
}

pub(super) fn photometric_name(photometric: Photometric) -> &'static str {
    match photometric {
        Photometric::Monochrome1 => "monochrome1",
        Photometric::Monochrome2 => "monochrome2",
        Photometric::Rgb => "rgb",
        Photometric::YbrFull422 => "ybr_full_422",
        _ => "unknown",
    }
}

pub(super) fn check_budget(count: u64, size: u64, limit: u64, label: &str) -> PyResult<()> {
    if count.checked_mul(size).is_none_or(|bytes| bytes > limit) {
        return Err(PyValueError::new_err(format!(
            "{label} exceeds the {limit}-byte limit"
        )));
    }
    Ok(())
}

pub(super) fn validate_region(
    layout: &TileLayout,
    (x, y, width, height): (i64, i64, u32, u32),
    limits: SlideLimits,
) -> PyResult<()> {
    if width == 0 || height == 0 {
        return Err(PyValueError::new_err(
            "region width and height must be positive",
        ));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > limits.region_pixels() {
        return Err(PyValueError::new_err(
            "region exceeds the slide's pixel limit",
        ));
    }
    let count = match layout {
        TileLayout::Regular {
            tile_width,
            tile_height,
            ..
        } => grid_bound(width, height, *tile_width, *tile_height)?,
        TileLayout::WholeLevel {
            virtual_tile_width,
            virtual_tile_height,
            ..
        } => grid_bound(width, height, *virtual_tile_width, *virtual_tile_height)?,
        TileLayout::Irregular {
            tile_advance: (dx, dy),
            extra_tiles: (top, bottom, left, right),
            ..
        } => {
            // Bound both iteration work and output before calling the shared
            // geometry routine; leave headroom for its signed extra-tile offsets.
            let mut count = 1_u64;
            for (origin, extent, advance, before, after) in [
                (x, width, *dx, *left, *right),
                (y, height, *dy, *top, *bottom),
            ] {
                let start = origin as f64 / advance;
                let end = (origin as f64 + f64::from(extent)) / advance;
                let margin = f64::from(u32::MAX) + 2.0;
                if !advance.is_finite()
                    || advance <= 0.0
                    || !start.is_finite()
                    || !end.is_finite()
                    || start <= i64::MIN as f64 + margin
                    || end >= i64::MAX as f64 - margin
                {
                    return Err(PyValueError::new_err(
                        "region cannot be represented on this irregular tile grid",
                    ));
                }
                let span = (f64::from(extent) / advance).ceil()
                    + f64::from(before)
                    + f64::from(after)
                    + 2.0;
                if span > limits.tile_index_bytes() as f64 {
                    return Err(PyValueError::new_err(
                        "region covers too many irregular tile positions",
                    ));
                }
                count = count
                    .checked_mul(span as u64)
                    .ok_or_else(|| PyValueError::new_err("tile count overflow"))?;
            }
            count
        }
        _ => return Err(PyValueError::new_err("unsupported tile layout")),
    };
    check_budget(
        count,
        std::mem::size_of::<wsi_rs::TileHit>() as u64,
        limits.tile_index_bytes(),
        "region tile metadata",
    )
}

fn grid_bound(width: u32, height: u32, tile_width: u32, tile_height: u32) -> PyResult<u64> {
    if tile_width == 0 || tile_height == 0 {
        return Err(PyValueError::new_err("invalid tile dimensions"));
    }
    (u64::from(width).div_ceil(u64::from(tile_width)) + 1)
        .checked_mul(u64::from(height).div_ceil(u64::from(tile_height)) + 1)
        .ok_or_else(|| PyValueError::new_err("tile count overflow"))
}
