//! Admission wrapper for readers without their own encoded-size planning.
use super::*;

pub(crate) struct ConservativeManagedReader {
    inner: Box<dyn SlideReader>,
    encoded_unit_bytes: u64,
    region_fastpath_in_pool: bool,
}

impl ConservativeManagedReader {
    /// Wraps a custom reader, whose region fast path runs inside the process
    /// decode pool as its only worker context.
    pub(crate) fn new(inner: Box<dyn SlideReader>, encoded_unit_bytes: u64) -> Self {
        Self {
            inner,
            encoded_unit_bytes,
            region_fastpath_in_pool: true,
        }
    }

    /// Wraps a built-in reader. Built-ins enter the pool only for actual decode
    /// work, so a declined or cached fast path never pays a pool round trip.
    pub(crate) fn builtin(inner: Box<dyn SlideReader>, encoded_unit_bytes: u64) -> Self {
        Self {
            region_fastpath_in_pool: false,
            ..Self::new(inner, encoded_unit_bytes)
        }
    }
}

impl SlideReader for ConservativeManagedReader {
    forward_slide_reader_methods!(inner);

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_tile_cpu(req)
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.inner.read_tiles_cpu(reqs)
    }

    fn read_tiles_cpu_controlled(
        &self,
        reqs: &[TileRequest],
        control: &crate::ReadControl,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.inner.read_tiles_cpu_controlled(reqs, control)
    }

    fn read_region_fastpath(
        &self,
        ctx: &mut SlideReadContext<'_>,
        req: &RegionRequest,
    ) -> Option<Result<CpuTile, WsiError>> {
        // Preserve the worker context of custom readers; built-ins decide
        // whether their fast path needs CPU work before entering the pool.
        if !self.region_fastpath_in_pool {
            return self.inner.read_region_fastpath(ctx, req);
        }
        crate::core::decode_runtime::DecodeRuntime::default_arc()
            .install_codec_cpu(|| self.inner.read_region_fastpath(ctx, req))
    }

    fn read_region(&self, req: &RegionRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_region(req)
    }

    fn read_display_tile(&self, req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_display_tile(req)
    }
}

impl ManagedSlideReader for ConservativeManagedReader {
    fn tile_encoded_upper_bound(&self, _req: &TileRequest) -> Result<u64, WsiError> {
        Ok(self.encoded_unit_bytes)
    }

    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError> {
        Ok(if !reqs.is_empty() {
            self.encoded_unit_bytes
        } else {
            0
        })
    }

    fn display_tile_encoded_upper_bound(&self, _req: &TileViewRequest) -> Result<u64, WsiError> {
        Ok(self.encoded_unit_bytes)
    }

    fn associated_encoded_upper_bound(&self, _name: &str) -> Result<u64, WsiError> {
        Ok(self.encoded_unit_bytes)
    }

    fn region_fastpath_encoded_upper_bound(&self, _req: &RegionRequest) -> Result<u64, WsiError> {
        Ok(self.encoded_unit_bytes)
    }
}
