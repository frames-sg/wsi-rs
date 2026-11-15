use super::*;
use crate::core::read_control::check_cancelled_opt;

impl AdaptiveDecodeReader {
    pub(crate) fn new_managed(
        inner: Box<dyn ManagedSlideReader>,
        runtime: Arc<DecodeRuntime>,
    ) -> Self {
        Self { inner, runtime }
    }

    /// Every acceleration policy decodes CPU-returning reads on the CPU.
    fn read_tiles_on_cpu(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        check_cancelled_opt(control)?;
        let operation = || match control {
            Some(control) => self.inner.read_tiles_cpu_controlled(reqs, control),
            None => self.inner.read_tiles_cpu(reqs),
        };
        let result = if batch_uses_jp2k(self.inner.as_ref(), reqs) {
            self.inner
                .read_tiles_cpu_fastpath(reqs, control)
                .unwrap_or_else(|| self.runtime.install_codec_cpu(operation))
        } else if rayon::current_thread_index().is_some() {
            // Other batches, such as JPEG, fan out on the invoking pool too.
            // JPEG XR decodes also dispatch through the codec pool themselves.
            crate::core::batch::run_codec_work_on_worker(operation)
        } else {
            operation()
        };
        check_cancelled_opt(control)?;
        result.and_then(|tiles| {
            crate::core::batch::expect_exact_count(tiles, reqs.len(), "adaptive CPU tile batch")
        })
    }
}

impl SlideReader for AdaptiveDecodeReader {
    crate::core::registry::forward_slide_reader_methods!(inner);

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        crate::core::batch::exactly_one(
            self.read_tiles_on_cpu(std::slice::from_ref(req), None)?,
            "adaptive single tile read",
        )
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_on_cpu(reqs, None)
    }

    fn read_tiles_cpu_controlled(
        &self,
        reqs: &[TileRequest],
        control: &crate::ReadControl,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_on_cpu(reqs, Some(control))
    }

    fn read_region_fastpath(
        &self,
        ctx: &mut crate::core::registry::SlideReadContext<'_>,
        req: &crate::core::types::RegionRequest,
    ) -> Option<Result<CpuTile, WsiError>> {
        self.inner.read_region_fastpath(ctx, req)
    }

    fn read_region(&self, req: &crate::core::types::RegionRequest) -> Result<CpuTile, WsiError> {
        self.runtime
            .install_codec_cpu(|| self.inner.read_region(req))
    }

    fn read_display_tile(
        &self,
        req: &crate::core::types::TileViewRequest,
    ) -> Result<CpuTile, WsiError> {
        self.runtime
            .install_codec_cpu(|| self.inner.read_display_tile(req))
    }
}

impl ManagedSlideReader for AdaptiveDecodeReader {
    #[cfg(feature = "metal")]
    fn read_metal_with_context(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::metal::MetalBackendSessions,
        context: &crate::core::limits::ReadExecutionContext<'_>,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        self.inner.read_metal_with_context(reqs, session, context)
    }

    fn read_tiles_with_context(
        &self,
        reqs: &[TileRequest],
        context: &crate::core::limits::ReadExecutionContext<'_>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_on_cpu(reqs, context.control)
    }

    fn tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError> {
        self.inner.tile_encoded_upper_bound(req)
    }

    fn raw_tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError> {
        self.inner.raw_tile_encoded_upper_bound(req)
    }

    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError> {
        self.inner.tile_batch_encoded_upper_bound(reqs)
    }

    fn display_tile_encoded_upper_bound(
        &self,
        req: &crate::core::types::TileViewRequest,
    ) -> Result<u64, WsiError> {
        self.inner.display_tile_encoded_upper_bound(req)
    }

    fn associated_encoded_upper_bound(&self, name: &str) -> Result<u64, WsiError> {
        self.inner.associated_encoded_upper_bound(name)
    }

    fn region_fastpath_encoded_upper_bound(
        &self,
        req: &crate::core::types::RegionRequest,
    ) -> Result<u64, WsiError> {
        self.inner.region_fastpath_encoded_upper_bound(req)
    }
}

fn batch_uses_jp2k(reader: &dyn SlideReader, reqs: &[TileRequest]) -> bool {
    reqs.iter().any(|request| {
        matches!(
            reader.tile_codec_kind(request),
            TileCodecKind::Jp2k | TileCodecKind::Htj2k
        )
    })
}
