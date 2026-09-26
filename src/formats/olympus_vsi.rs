use std::path::Path;
use std::sync::Arc;

use j2k_core::BackendRequest;

use crate::core::registry::{
    BackendOpenConfig, ConfiguredDatasetReader, ConfiguredFormatProbe, DatasetReader, FormatProbe,
    ManagedSlideReader, ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::{
    AxesShape, CpuTile, Dataset, PlaneSelection, RawCompressedTile, TileCodecKind, TileRequest,
};
use crate::error::WsiError;

mod pixels;
mod scene;
mod slide;

use scene::EtsTileKey;
use slide::{companion_dir, OlympusVsiSlide};

pub(crate) struct OlympusVsiBackend;

impl FormatProbe for OlympusVsiBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        let detected = is_vsi_path(path) && companion_dir(path).is_some_and(|dir| dir.is_dir());
        if detected {
            return Ok(ProbeResult::detected("olympus", ProbeConfidence::Definite));
        }
        // Preserve the existing externally observable negative confidence.
        Ok(ProbeResult {
            detected: false,
            vendor: String::new(),
            confidence: ProbeConfidence::Definite,
        })
    }
}

impl ConfiguredFormatProbe for OlympusVsiBackend {}

impl DatasetReader for OlympusVsiBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let reader = self.open_with_config(path, BackendOpenConfig::deterministic())?;
        Ok(reader)
    }
}

impl ConfiguredDatasetReader for OlympusVsiBackend {
    fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError> {
        let reader = Box::new(OlympusVsiReader {
            slide: Arc::new(OlympusVsiSlide::parse_with_config(path, config)?),
        });
        Ok(reader)
    }
}

struct OlympusVsiReader {
    slide: Arc<OlympusVsiSlide>,
}

impl SlideReader for OlympusVsiReader {
    fn dataset(&self) -> &Dataset {
        &self.slide.dataset
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        let mut output = vec![None; reqs.len()];
        let mut jobs = Vec::new();
        let mut slots = Vec::new();
        let mut unique = std::collections::HashMap::new();
        let mut duplicates = Vec::new();
        for (slot, req) in reqs.iter().enumerate() {
            let (scene, level, tile) = self.tile_for_request(req)?;
            let key = (req.scene.get(), tile_key(req));
            if let Some(&source_slot) = unique.get(&key) {
                duplicates.push((slot, source_slot));
                continue;
            }
            unique.insert(key, slot);
            if let Some(tile) = tile {
                jobs.push(
                    scene
                        .prepare_tile(tile, BackendRequest::Cpu)
                        .map_err(|err| tile_error(req, err))?,
                );
                slots.push(slot);
            } else {
                output[slot] = Some(scene.background_tile(level.tile_width, level.tile_height)?);
            }
        }
        let decoded = crate::decode::jp2k::decode_batch_jp2k(&jobs);
        let decoded =
            crate::core::batch::expect_exact_count(decoded, slots.len(), "Olympus ETS batch")?;
        for (slot, tile) in slots.into_iter().zip(decoded) {
            output[slot] = Some(tile.map_err(|err| tile_error(&reqs[slot], err))?);
        }
        for (slot, source_slot) in duplicates {
            output[slot] = output[source_slot].clone();
        }
        Ok(output
            .into_iter()
            .map(|tile| tile.expect("every ETS request has an output slot"))
            .collect())
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Cpu)
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        // Sparse tiles are synthesized from the ETS background without a codec.
        match self.tile_for_request(req) {
            Ok((_, _, Some(_))) => TileCodecKind::Jp2k,
            Ok((_, _, None)) | Err(_) => TileCodecKind::Other,
        }
    }

    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        let (scene, _, tile) = self.tile_for_request(req)?;
        let tile = tile.ok_or_else(|| WsiError::Unsupported {
            reason: "J2K passthrough is not available for sparse ETS background tiles".into(),
        })?;
        scene.raw_compressed_tile(tile)
    }

    #[cfg(feature = "metal")]
    fn read_tiles_metal(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::metal::MetalBackendSessions,
    ) -> Result<Vec<crate::output::metal::MetalDeviceTile>, WsiError> {
        let stored = self.stored_jp2k_tiles(reqs, None)?;
        crate::decode::jp2k::decode_batch_jp2k_metal(&stored.jobs(BackendRequest::Metal), sessions)
            .into_iter()
            .zip(reqs)
            .map(|(result, req)| result.map_err(|err| tile_error(req, err)))
            .collect()
    }

    #[cfg(feature = "cuda")]
    fn read_tiles_cuda(
        &self,
        reqs: &[TileRequest],
        sessions: &crate::output::cuda::CudaBackendSessions,
    ) -> Result<Vec<crate::output::cuda::CudaDeviceTile>, WsiError> {
        let stored = self.stored_jp2k_tiles(reqs, None)?;
        crate::decode::jp2k::decode_batch_jp2k_cuda(&stored.jobs(BackendRequest::Cuda), sessions)
            .into_iter()
            .zip(reqs)
            .map(|(result, req)| result.map_err(|err| tile_error(req, err)))
            .collect()
    }
}

/// Stored ETS codestreams for strict device and adaptive reads.
#[cfg(any(feature = "metal", feature = "cuda"))]
struct StoredJp2kTiles<'a> {
    payloads: Vec<Vec<u8>>,
    /// The scene and payload index of each request, in request order.
    slots: Vec<(&'a scene::EtsScene, usize)>,
}

#[cfg(any(feature = "metal", feature = "cuda"))]
impl StoredJp2kTiles<'_> {
    fn jobs(&self, backend: BackendRequest) -> Vec<crate::decode::jp2k::Jp2kDecodeJob<'_>> {
        self.slots
            .iter()
            .map(|(scene, index)| scene.jp2k_job(&self.payloads[*index], backend))
            .collect()
    }
}

impl OlympusVsiReader {
    /// Reads each distinct stored tile once. Sparse background tiles have no
    /// codestream, so device reads reject them rather than upload host pixels.
    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn stored_jp2k_tiles(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
    ) -> Result<StoredJp2kTiles<'_>, WsiError> {
        use std::collections::hash_map::Entry;
        let mut unique = std::collections::HashMap::new();
        let mut payloads = Vec::new();
        let mut slots = Vec::with_capacity(reqs.len());
        for req in reqs {
            if let Some(control) = control {
                control.check_cancelled()?;
            }
            let (scene, _, tile) = self.tile_for_request(req)?;
            let tile = tile.ok_or_else(|| WsiError::Unsupported {
                reason: "device backend not available for sparse ETS background tile".into(),
            })?;
            let index = match unique.entry((req.scene.get(), tile_key(req))) {
                Entry::Occupied(entry) => *entry.get(),
                Entry::Vacant(entry) => {
                    payloads.push(
                        scene
                            .read_payload(tile)
                            .map_err(|err| tile_error(req, err))?,
                    );
                    *entry.insert(payloads.len() - 1)
                }
            };
            slots.push((scene, index));
        }
        Ok(StoredJp2kTiles { payloads, slots })
    }

    fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let (scene, level, tile) = self.tile_for_request(req)?;
        match tile {
            Some(tile) => scene
                .decode_tile(tile, backend)
                .map_err(|err| tile_error(req, err)),
            None => scene.background_tile(level.tile_width, level.tile_height),
        }
    }

    fn tile_for_request(
        &self,
        req: &TileRequest,
    ) -> Result<(&scene::EtsScene, &scene::EtsLevel, Option<&scene::EtsTile>), WsiError> {
        let scene = self
            .slide
            .scenes
            .get(req.scene.get())
            .ok_or(WsiError::SceneOutOfRange {
                index: req.scene.get(),
                count: self.slide.scenes.len(),
            })?;
        if req.series.get() != 0 {
            return Err(WsiError::SeriesOutOfRange {
                index: req.series.get(),
                count: 1,
            });
        }
        let level =
            scene
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: scene.levels.len() as u32,
                })?;
        validate_plane(req.plane.get(), scene.axes)?;
        if req.col < 0
            || req.row < 0
            || req.col >= level.tiles_across as i64
            || req.row >= level.tiles_down as i64
        {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "tile ({},{}) out of range ({}x{})",
                    req.col, req.row, level.tiles_across, level.tiles_down
                ),
            });
        }

        let key = tile_key(req);
        Ok((scene, level, scene.tiles.get(&key)))
    }
}

fn tile_key(req: &TileRequest) -> EtsTileKey {
    EtsTileKey {
        level: req.level.get(),
        z: req.plane.get().z,
        c: req.plane.get().c,
        t: req.plane.get().t,
        col: req.col as u32,
        row: req.row as u32,
    }
}

fn tile_error(req: &TileRequest, err: WsiError) -> WsiError {
    WsiError::TileRead {
        col: req.col,
        row: req.row,
        level: req.level.get(),
        reason: err.to_string(),
    }
}

impl ManagedSlideReader for OlympusVsiReader {
    #[cfg(any(feature = "metal", feature = "cuda"))]
    fn prepare_adaptive_jp2k(
        &self,
        reqs: &[TileRequest],
        workers: usize,
        control: Option<&crate::ReadControl>,
    ) -> Option<Result<crate::decode::jp2k::PreparedJp2kBatch, WsiError>> {
        Some(self.stored_jp2k_tiles(reqs, control).and_then(|stored| {
            crate::decode::jp2k::PreparedJp2kBatch::new(&stored.jobs(BackendRequest::Cpu), workers)
        }))
    }

    fn tile_encoded_upper_bound(&self, req: &TileRequest) -> Result<u64, WsiError> {
        Ok(self
            .tile_for_request(req)?
            .2
            .map_or(0, |tile| u64::from(tile.byte_count)))
    }
    fn tile_batch_encoded_upper_bound(&self, reqs: &[TileRequest]) -> Result<u64, WsiError> {
        let mut unique = std::collections::HashSet::new();
        reqs.iter().try_fold(0_u64, |sum, req| {
            let tile = self.tile_for_request(req)?.2;
            if !unique.insert((req.scene.get(), tile_key(req))) {
                return Ok(sum);
            }
            Ok(sum.saturating_add(tile.map_or(0, |tile| u64::from(tile.byte_count))))
        })
    }
    fn display_tile_encoded_upper_bound(
        &self,
        _: &crate::TileViewRequest,
    ) -> Result<u64, WsiError> {
        Ok(self.slide.scenes[0].encoded_unit_limit)
    }
    fn associated_encoded_upper_bound(&self, _: &str) -> Result<u64, WsiError> {
        Ok(self.slide.scenes[0].encoded_unit_limit)
    }
    fn region_fastpath_encoded_upper_bound(
        &self,
        _: &crate::RegionRequest,
    ) -> Result<u64, WsiError> {
        Ok(self.slide.scenes[0].encoded_unit_limit)
    }
}

fn validate_plane(plane: PlaneSelection, axes: AxesShape) -> Result<(), WsiError> {
    if plane.z >= axes.z {
        return Err(WsiError::PlaneOutOfRange {
            axis: "z".into(),
            value: plane.z,
            max: axes.z.saturating_sub(1),
        });
    }
    if plane.c >= axes.c {
        return Err(WsiError::PlaneOutOfRange {
            axis: "c".into(),
            value: plane.c,
            max: axes.c.saturating_sub(1),
        });
    }
    if plane.t >= axes.t {
        return Err(WsiError::PlaneOutOfRange {
            axis: "t".into(),
            value: plane.t,
            max: axes.t.saturating_sub(1),
        });
    }
    Ok(())
}

fn is_vsi_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some(ext) if ext.eq_ignore_ascii_case("vsi")
    )
}

fn invalid_slide(path: &Path, message: impl Into<String>) -> WsiError {
    WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;
