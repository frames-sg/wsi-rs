//! Output estimates, work sizes and limit checks for slide reads.
use super::*;
use crate::core::limits::ensure_within;

impl Slide {
    pub(super) fn estimate_tile_output_bytes(&self, req: &TileRequest) -> Result<u64, WsiError> {
        let (_, series, level) = self.dataset().level(req.scene, req.series, req.level)?;
        let dimensions = match &level.tile_layout {
            TileLayout::Regular {
                tile_width,
                tile_height,
                ..
            } => (*tile_width, *tile_height),
            TileLayout::WholeLevel {
                virtual_tile_width,
                virtual_tile_height,
                ..
            } => (*virtual_tile_width, *virtual_tile_height),
            TileLayout::Irregular { tiles, .. } => tiles
                .get(&(req.col, req.row))
                .map(|entry| entry.dimensions)
                .ok_or_else(|| WsiError::tile_read(req, "tile not found"))?,
        };
        let channels = u64::try_from(series.channels.len().max(4)).unwrap_or(u64::MAX);
        let output_bytes = u64::from(dimensions.0)
            .checked_mul(u64::from(dimensions.1))
            .and_then(|pixels| pixels.checked_mul(channels))
            .and_then(|samples| samples.checked_mul(series.sample_type.byte_size() as u64))
            .ok_or(WsiError::ResourceLimit {
                resource: "decoded tile/associated output",
                requested: u64::MAX,
                limit: self.limits.decoded_output_bytes(),
            })?;
        self.check_output_limit(output_bytes, "decoded tile/associated output")?;
        Ok(output_bytes)
    }

    pub(super) fn estimate_associated_output_bytes(&self, name: &str) -> Result<u64, WsiError> {
        let Some(image) = self.dataset().associated_images.get(name) else {
            return Ok(0);
        };
        u64::from(image.dimensions.0)
            .checked_mul(u64::from(image.dimensions.1))
            .and_then(|pixels| pixels.checked_mul(u64::from(image.channels)))
            .and_then(|samples| samples.checked_mul(image.sample_type.byte_size() as u64))
            .ok_or(WsiError::ResourceLimit {
                resource: "decoded tile/associated output",
                requested: u64::MAX,
                limit: self.limits.decoded_output_bytes(),
            })
    }

    pub(super) fn check_region_output(&self, req: &RegionRequest) -> Result<u64, WsiError> {
        // Two u32 factors cannot overflow u64.
        let pixels = ensure_within(
            "region pixels",
            u64::from(req.size_px.0) * u64::from(req.size_px.1),
            self.limits.region_pixels(),
        )?;
        let bytes = pixels.checked_mul(4).ok_or(WsiError::ResourceLimit {
            resource: "region RGBA output",
            requested: u64::MAX,
            limit: self.limits.region_rgba_bytes(),
        })?;
        ensure_within("region RGBA output", bytes, self.limits.region_rgba_bytes())
    }

    pub(super) fn region_work_bytes(
        &self,
        encoded_bytes: u64,
        output_bytes: u64,
        largest_source_bytes: u64,
        fractional: bool,
    ) -> Result<u64, WsiError> {
        let staging = output_bytes.max(largest_source_bytes);
        let mut requested = encoded_bytes
            .checked_add(output_bytes)
            .and_then(|bytes| bytes.checked_add(staging))
            .unwrap_or(u64::MAX);
        if fractional {
            requested = requested.saturating_add(output_bytes);
        }
        ensure_within(
            "per-operation transient work",
            requested,
            self.limits.operation_transient_bytes(),
        )
    }

    pub(super) fn ordinary_work_bytes(
        &self,
        encoded_bytes: u64,
        output_bytes: u64,
    ) -> Result<u64, WsiError> {
        ReadWork::new(encoded_bytes, output_bytes)
            .ordinary_bytes(self.limits.operation_transient_bytes())
    }

    pub(super) fn check_output_limit(
        &self,
        bytes: u64,
        resource: &'static str,
    ) -> Result<(), WsiError> {
        ensure_within(resource, bytes, self.limits.decoded_output_bytes()).map(drop)
    }

    pub(super) fn validate_decoded_output(
        &self,
        tile: &CpuTile,
        resource: &'static str,
    ) -> Result<(), WsiError> {
        tile.validate_invariants()?;
        self.check_output_limit(
            u64::try_from(tile.data().byte_size()).unwrap_or(u64::MAX),
            resource,
        )
    }

    pub(super) fn validate_encoded_unit(&self, byte_len: usize) -> Result<(), WsiError> {
        ensure_within(
            "encoded tile/frame unit",
            u64::try_from(byte_len).unwrap_or(u64::MAX),
            self.limits.encoded_unit_bytes(),
        )
        .map(drop)
    }

    pub(crate) fn validate_encoded_contract(
        &self,
        byte_len: usize,
        promised: u64,
        context: &'static str,
    ) -> Result<(), WsiError> {
        let actual = u64::try_from(byte_len).unwrap_or(u64::MAX);
        if actual > promised {
            return Err(WsiError::BackendContract {
                context,
                expected: usize::try_from(promised).unwrap_or(usize::MAX),
                actual: byte_len,
            });
        }
        self.validate_encoded_unit(byte_len)
    }
}

pub(super) fn checked_rgba_bytes(
    width: u32,
    height: u32,
    resource: &'static str,
) -> Result<u64, WsiError> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(WsiError::ResourceLimit {
            resource,
            requested: u64::MAX,
            limit: u64::MAX,
        })
}

pub(super) fn check_subpixel_offset(offset_px: (f64, f64)) -> Result<(), WsiError> {
    let valid = |value: f64| value.is_finite() && (0.0..1.0).contains(&value);
    if valid(offset_px.0) && valid(offset_px.1) {
        Ok(())
    } else {
        Err(WsiError::DisplayConversion(format!(
            "subpixel offset must be finite and in [0, 1), got ({}, {})",
            offset_px.0, offset_px.1
        )))
    }
}
