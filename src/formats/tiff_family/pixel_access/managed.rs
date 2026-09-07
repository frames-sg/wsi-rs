use super::*;
use crate::core::registry::ManagedSlideReader;

impl ManagedSlideReader for TiffPixelReader {
    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn prepare_adaptive_jp2k(
        &self,
        reqs: &[TileRequest],
        workers: usize,
        control: Option<&crate::ReadControl>,
    ) -> Option<Result<crate::decode::jp2k::PreparedJp2kBatch, WsiError>> {
        Some(
            self.collect_tiled_ifd_jp2k_jobs(reqs, j2k_core::BackendRequest::Cpu, control)
                .and_then(|jobs| crate::decode::jp2k::PreparedJp2kBatch::new(&jobs, workers)),
        )
    }

    fn tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError> {
        let TileSource::TiledIfd {
            ifd_id,
            compression,
            jpeg_tables,
        } = self.tile_source_for(req)?
        else {
            return Ok(self.container.limits().encoded_unit_bytes());
        };
        let bytes = self.tiled_ifd_tile_span(req, *ifd_id)?.byte_count;
        // JPEG preparation can retain the original payload while reconstructing
        // a standalone frame with shared TIFF tables. JP2K consumes the payload.
        Ok(if *compression == Compression::Jpeg {
            bytes
                .saturating_mul(2)
                .saturating_add(jpeg_tables.as_ref().map_or(0, |tables| tables.len() as u64))
        } else {
            bytes
        })
    }

    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError> {
        let mut bytes = 0_u64;
        for req in reqs {
            if !matches!(self.tile_source_for(req)?, TileSource::TiledIfd { .. }) {
                // Preserve the established admission of non-tiled fast paths.
                return Ok(self.container.limits().encoded_unit_bytes());
            }
            bytes = bytes.saturating_add(self.tile_encoded_upper_bound(req)?);
        }
        Ok(bytes)
    }

    fn display_tile_encoded_upper_bound(&self, _: &TileViewRequest) -> Result<u64, WsiError> {
        Ok(self.container.limits().encoded_unit_bytes())
    }
    fn associated_encoded_upper_bound(&self, _: &str) -> Result<u64, WsiError> {
        Ok(self.container.limits().encoded_unit_bytes())
    }
    fn region_fastpath_encoded_upper_bound(&self, _: &RegionRequest) -> Result<u64, WsiError> {
        Ok(self.container.limits().encoded_unit_bytes())
    }
}
