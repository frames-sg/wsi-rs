use super::*;

impl AdaptiveDecodeReader {
    #[cfg(test)]
    pub(crate) fn new(inner: Box<dyn SlideReader>, runtime: Arc<DecodeRuntime>) -> Self {
        Self::new_managed(
            Box::new(ConservativeManagedReader::new(
                inner,
                crate::SlideLimits::default().encoded_unit_bytes(),
            )),
            runtime,
        )
    }

    pub(crate) fn new_managed(
        inner: Box<dyn ManagedSlideReader>,
        runtime: Arc<DecodeRuntime>,
    ) -> Self {
        Self { inner, runtime }
    }

    fn read_tiles_adaptive(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
        context: Option<&crate::core::limits::ReadExecutionContext<'_>>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        Self::check_control(control)?;
        let _ = context;
        if reqs.is_empty() || self.runtime.options.acceleration == DecodeAcceleration::CpuOnly {
            return self.read_inner_cpu(reqs, control);
        }
        #[cfg(any(feature = "metal", feature = "cuda"))]
        {
            self.read_tiles_adaptive_device(reqs, control, context)
        }
        #[cfg(not(any(feature = "metal", feature = "cuda")))]
        {
            self.read_inner_cpu(reqs, control)
        }
    }

    pub(super) fn check_control(control: Option<&crate::ReadControl>) -> Result<(), WsiError> {
        control.map_or(Ok(()), crate::ReadControl::check_cancelled)
    }

    pub(super) fn read_inner_cpu(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        Self::check_control(control)?;
        let operation = || match control {
            Some(control) => self.inner.read_tiles_cpu_controlled(reqs, control),
            None => self.inner.read_tiles_cpu(reqs),
        };
        let result = if batch_uses_jp2k(self.inner.as_ref(), reqs) {
            self.inner
                .read_tiles_cpu_fastpath(reqs, control)
                .unwrap_or_else(|| self.runtime.install_jp2k_cpu(operation))
        } else {
            operation()
        };
        Self::check_control(control)?;
        result.and_then(|tiles| {
            crate::core::batch::expect_exact_count(tiles, reqs.len(), "adaptive CPU tile batch")
        })
    }
}

impl SlideReader for AdaptiveDecodeReader {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        self.inner.tile_codec_kind(req)
    }

    fn level_source_kind(
        &self,
        scene: crate::core::types::SceneId,
        series: crate::core::types::SeriesId,
        level: crate::core::types::LevelIdx,
    ) -> Result<crate::core::types::LevelSourceKind, WsiError> {
        self.inner.level_source_kind(scene, series, level)
    }

    fn prepare_level_controlled(
        &self,
        scene: crate::core::types::SceneId,
        series: crate::core::types::SeriesId,
        level: crate::core::types::LevelIdx,
        control: &crate::ReadControl,
    ) -> Result<(), WsiError> {
        self.inner
            .prepare_level_controlled(scene, series, level, control)
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        crate::core::batch::exactly_one(
            self.read_tiles_adaptive(std::slice::from_ref(req), None, None)?,
            "adaptive single tile read",
        )
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_adaptive(reqs, None, None)
    }

    fn read_tiles_cpu_controlled(
        &self,
        reqs: &[TileRequest],
        control: &crate::ReadControl,
    ) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles_adaptive(reqs, Some(control), None)
    }

    #[cfg(feature = "metal")]
    fn read_tiles_metal(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        self.inner.read_tiles_metal(reqs, session)
    }

    #[cfg(feature = "cuda")]
    fn read_tiles_cuda(
        &self,
        reqs: &[TileRequest],
        session: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        self.inner.read_tiles_cuda(reqs, session)
    }

    fn read_raw_compressed_tile(
        &self,
        req: &TileRequest,
    ) -> Result<crate::core::types::RawCompressedTile, WsiError> {
        self.inner.read_raw_compressed_tile(req)
    }

    fn read_raw_compressed_display_tile(
        &self,
        req: &crate::core::types::TileViewRequest,
    ) -> Result<crate::core::types::RawCompressedTile, WsiError> {
        self.inner.read_raw_compressed_display_tile(req)
    }

    fn use_display_tile_cache(&self, req: &crate::core::types::TileViewRequest) -> bool {
        self.inner.use_display_tile_cache(req)
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
            .install_jp2k_cpu(|| self.inner.read_region(req))
    }

    fn read_display_tile(
        &self,
        req: &crate::core::types::TileViewRequest,
    ) -> Result<CpuTile, WsiError> {
        self.runtime
            .install_jp2k_cpu(|| self.inner.read_display_tile(req))
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.inner.read_associated(name)
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
        self.read_tiles_adaptive(reqs, context.control, Some(context))
    }

    fn tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError> {
        self.inner.tile_encoded_upper_bound(req)
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

pub(super) fn batch_uses_jp2k(reader: &dyn SlideReader, reqs: &[TileRequest]) -> bool {
    jp2k_tile_count(reader, reqs) != 0
}

pub(super) fn jp2k_tile_count(reader: &dyn SlideReader, reqs: &[TileRequest]) -> usize {
    reqs.iter()
        .filter(|request| {
            matches!(
                reader.tile_codec_kind(request),
                TileCodecKind::Jp2k | TileCodecKind::Htj2k
            )
        })
        .count()
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
pub(super) fn route_key_for_batch(
    reader: &dyn SlideReader,
    reqs: &[TileRequest],
    device_identity: &str,
) -> Option<DecodeRouteKey> {
    let first = reqs.first()?;
    if !reqs.iter().all(|request| {
        request.scene == first.scene
            && request.series == first.series
            && request.level == first.level
    }) {
        return None;
    }
    let codec_kind = reader.tile_codec_kind(first);
    if !matches!(codec_kind, TileCodecKind::Jp2k | TileCodecKind::Htj2k)
        || !reqs
            .iter()
            .all(|request| reader.tile_codec_kind(request) == codec_kind)
    {
        return None;
    }
    let level = dataset_level(
        reader.dataset(),
        first.scene.get(),
        first.series.get(),
        first.level.get(),
    )?;
    let dimensions = reqs
        .iter()
        .map(|request| logical_tile_dimensions(level, request))
        .collect::<Option<Vec<_>>>()?;
    let sample_geometry = RouteSampleGeometry::from_dimensions(dimensions);
    Some(DecodeRouteKey {
        dataset_id: reader.dataset().id.0,
        scene: first.scene.get(),
        series: first.series.get(),
        level: first.level.get(),
        sample_geometry,
        codec_kind,
        device_identity: device_identity.to_owned(),
        sample_tile_count: reqs.len(),
        cpu_workers: DecodeRuntime::default_arc().cpu_worker_count(),
    })
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
pub(super) fn dataset_level(
    dataset: &Dataset,
    scene: usize,
    series: usize,
    level: u32,
) -> Option<&Level> {
    dataset
        .scenes
        .get(scene)?
        .series
        .get(series)?
        .levels
        .get(level as usize)
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
pub(super) fn logical_tile_dimensions(level: &Level, request: &TileRequest) -> Option<(u32, u32)> {
    match &level.tile_layout {
        TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } => {
            let col = u64::try_from(request.col).ok()?;
            let row = u64::try_from(request.row).ok()?;
            if col >= *tiles_across || row >= *tiles_down {
                return None;
            }
            let x = col.checked_mul(u64::from(*tile_width))?;
            let y = row.checked_mul(u64::from(*tile_height))?;
            let width = level
                .dimensions
                .0
                .checked_sub(x)?
                .min(u64::from(*tile_width));
            let height = level
                .dimensions
                .1
                .checked_sub(y)?
                .min(u64::from(*tile_height));
            Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
        }
        _ => None,
    }
}
