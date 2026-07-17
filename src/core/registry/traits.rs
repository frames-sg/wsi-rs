use super::*;
use j2k_core::BackendRequest;

// ── Probe traits ───────────────────────────────────────────────────

/// Detects whether a file is a given format. Fast, no full parse.
pub trait FormatProbe: Send + Sync {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError>;
}

/// Result from a cheap file-format probe.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProbeResult {
    pub detected: bool,
    pub vendor: String,
    pub confidence: ProbeConfidence,
}

impl ProbeResult {
    /// Creates a positive probe result for a detected vendor.
    pub fn detected(vendor: impl Into<String>, confidence: ProbeConfidence) -> Self {
        Self {
            detected: true,
            vendor: vendor.into(),
            confidence,
        }
    }

    /// Creates a negative probe result for a vendor that did not match.
    ///
    /// The registry ignores `confidence` when `detected` is false.
    pub fn not_detected(vendor: impl Into<String>) -> Self {
        Self {
            detected: false,
            vendor: vendor.into(),
            confidence: ProbeConfidence::Likely,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProbeConfidence {
    Definite,
    Likely,
}

/// Opens a file and returns a SlideReader.
pub trait DatasetReader: Send + Sync {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError>;
}

// ── Read interface ─────────────────────────────────────────────────

pub(crate) fn read_cpu_tiles_with_backend(
    reqs: &[TileRequest],
    output: TileOutputPreference,
    require_device_reason: &'static str,
    mut read_tile: impl FnMut(&TileRequest, BackendRequest) -> Result<CpuTile, WsiError>,
) -> Result<Vec<TilePixels>, WsiError> {
    let backend = match output {
        TileOutputPreference::Cpu { backend }
        | TileOutputPreference::PreferDevice { backend, .. } => backend.to_j2k(),
        TileOutputPreference::RequireDevice { .. } => {
            return Err(WsiError::Unsupported {
                reason: require_device_reason.into(),
            });
        }
    };
    reqs.iter()
        .map(|req| read_tile(req, backend).map(TilePixels::Cpu))
        .collect()
}

pub struct SlideReadContext<'a> {
    tile_cache: Option<&'a TileCache>,
    output: TileOutputPreference,
    max_region_pixels: u64,
}

impl<'a> SlideReadContext<'a> {
    pub(crate) fn new(
        tile_cache: Option<&'a TileCache>,
        output: TileOutputPreference,
        max_region_pixels: u64,
    ) -> Self {
        Self {
            tile_cache,
            output,
            max_region_pixels,
        }
    }

    pub(crate) fn tile_cache(&self) -> Option<&'a TileCache> {
        self.tile_cache
    }

    pub fn output(&self) -> &TileOutputPreference {
        &self.output
    }

    pub fn max_region_pixels(&self) -> u64 {
        self.max_region_pixels
    }
}

/// Phase-2 read interface.
///
/// `read_tile` is a default impl over a 1-element slice into `read_tiles`. A
/// backend that overrides `read_tiles` automatically gets the right
/// `read_tile` for free:
///
/// ```
/// use wsi_rs::{
///     ColorSpace, CpuTile, Dataset, SlideReader, TileOutputPreference, TilePixels, TileRequest,
///     WsiError,
/// };
/// # fn _example() {
/// struct Mock;
/// impl SlideReader for Mock {
///     fn dataset(&self) -> &Dataset { unimplemented!() }
///     fn read_tiles(
///         &self,
///         reqs: &[TileRequest],
///         _: TileOutputPreference,
///     ) -> Result<Vec<TilePixels>, WsiError> {
///         Ok(reqs
///             .iter()
///             .map(|_| {
///                 TilePixels::Cpu(
///                     CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![255, 0, 0])
///                         .unwrap(),
///                 )
///             })
///             .collect())
///     }
///     fn read_tile_cpu(&self, _: &TileRequest) -> Result<CpuTile, WsiError> {
///         Ok(CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![255, 0, 0]).unwrap())
///     }
/// }
/// let m = Mock;
/// let _ = m.read_tile(
///     &TileRequest::new(0usize, 0usize, 0, 0, 0),
///     TileOutputPreference::cpu(),
/// );
/// # }
/// ```
pub trait SlideReader: Send + Sync {
    fn dataset(&self) -> &Dataset;
    fn tile_codec_kind(&self, _req: &TileRequest) -> TileCodecKind {
        TileCodecKind::Other
    }
    fn level_source_kind(
        &self,
        scene: SceneId,
        series: SeriesId,
        level: LevelIdx,
    ) -> Result<LevelSourceKind, WsiError> {
        let dataset = self.dataset();
        let scene_ref = dataset
            .scenes
            .get(scene.get())
            .ok_or(WsiError::SceneOutOfRange {
                index: scene.get(),
                count: dataset.scenes.len(),
            })?;
        let series_ref = scene_ref
            .series
            .get(series.get())
            .ok_or(WsiError::SeriesOutOfRange {
                index: series.get(),
                count: scene_ref.series.len(),
            })?;
        if level.get() as usize >= series_ref.levels.len() {
            return Err(WsiError::LevelOutOfRange {
                level: level.get(),
                count: series_ref.levels.len() as u32,
            });
        }
        Ok(LevelSourceKind::Physical)
    }
    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        if matches!(output, TileOutputPreference::RequireDevice { .. }) {
            return Err(WsiError::Unsupported {
                reason: "RequireDevice not supported by this reader in Phase 2".into(),
            });
        }
        reqs.iter()
            .map(|req| self.read_tile_cpu(req).map(TilePixels::Cpu))
            .collect()
    }
    fn read_tiles_controlled(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
        control: &crate::ReadControl,
    ) -> Result<Vec<TilePixels>, WsiError> {
        control.check_cancelled()?;
        let mut tiles = Vec::with_capacity(reqs.len());
        for request in reqs {
            control.check_cancelled()?;
            tiles.push(self.read_tile(request, output.clone())?);
            control.check_cancelled()?;
        }
        Ok(tiles)
    }
    fn read_tile(
        &self,
        req: &TileRequest,
        output: TileOutputPreference,
    ) -> Result<TilePixels, WsiError> {
        let mut tiles = self.read_tiles(std::slice::from_ref(req), output)?;
        match tiles.len() {
            1 => Ok(tiles.remove(0)),
            0 => Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "empty tile batch result".into(),
            }),
            count => Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!("single tile read returned {count} tiles"),
            }),
        }
    }
    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError>;
    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        Err(WsiError::Unsupported {
            reason: format!(
                "raw compressed tile access is not available for tile ({}, {}) at level {}",
                req.col,
                req.row,
                req.level.get()
            ),
        })
    }
    fn read_raw_compressed_display_tile(
        &self,
        req: &TileViewRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        Err(WsiError::Unsupported {
            reason: format!(
                "raw compressed display tile access is not available for tile ({}, {}) at level {}",
                req.col,
                req.row,
                req.level.get()
            ),
        })
    }
    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.read_tiles(reqs, TileOutputPreference::cpu())?
            .into_iter()
            .map(|tile| match tile {
                TilePixels::Cpu(cpu) => Ok(cpu),
                TilePixels::Device(_) => Err(WsiError::Unsupported {
                    reason: "CPU tile request returned device payload".into(),
                }),
            })
            .collect()
    }
    fn use_display_tile_cache(&self, _req: &TileViewRequest) -> bool {
        true
    }
    fn read_region_fastpath(
        &self,
        _ctx: &mut SlideReadContext<'_>,
        _req: &RegionRequest,
    ) -> Option<Result<CpuTile, WsiError>> {
        None
    }
    fn read_region(
        &self,
        req: &RegionRequest,
        output: TileOutputPreference,
    ) -> Result<TilePixels, WsiError> {
        if matches!(output, TileOutputPreference::RequireDevice { .. }) {
            return Err(WsiError::Unsupported {
                reason: "region requires CPU composition; RequireDevice not supported in Phase 2"
                    .into(),
            });
        }
        composite_region_from_source(self, None, req, DEFAULT_MAX_REGION_PIXELS)
            .map(TilePixels::Cpu)
    }
    fn read_display_tile(&self, req: &TileViewRequest) -> Result<CpuTile, WsiError> {
        read_display_tile_from_source(self, None, req, TileOutputPreference::cpu())
    }
    fn associated_image(&self, name: &str) -> Result<Option<CpuTile>, WsiError> {
        match self.read_associated(name) {
            Ok(tile) => Ok(Some(tile)),
            Err(WsiError::AssociatedImageNotFound(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }
    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        Err(WsiError::AssociatedImageNotFound(name.into()))
    }
    fn recommended_shared_cache_bytes(&self) -> Option<u64> {
        None
    }
}
