use super::model::VmsSlide;
use super::*;

const MAX_VMS_JPEG_SHARDS: u64 = 65_536;

mod parse;

pub(super) struct VmsReader {
    pub(super) slide: Arc<VmsSlide>,
}

impl SlideReader for VmsReader {
    fn dataset(&self) -> &Dataset {
        &self.slide.dataset
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        read_cpu_tiles_with_backend(
            reqs,
            output,
            "RequireDevice is not supported for VMS",
            |req, backend| self.read_tile_with_backend(req, backend),
        )
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Auto)
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        if let Some(cached) = self
            .slide
            .associated_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
        {
            return Ok(cached.as_ref().clone());
        }
        let path = self
            .slide
            .associated_paths
            .get(name)
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        let data = read_file_bounded(path, MAX_COMPRESSED_INPUT_BYTES, "VMS associated JPEG")
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.clone(),
            })?;
        crate::core::batch::exactly_one(
            decode_batch_jpeg(&[JpegDecodeJob {
                data: Cow::Borrowed(&data),
                tables: None,
                expected_width: 0,
                expected_height: 0,
                color_transform: j2k_jpeg::ColorTransform::Auto,
                force_dimensions: false,
                requested_size: None,
            }]),
            "VMS associated JPEG decode",
        )?
        .map(|tile| {
            let tile = Arc::new(tile);
            self.slide
                .associated_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .put(name.to_string(), tile.clone());
            tile.as_ref().clone()
        })
    }
}

impl VmsReader {
    fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let scene =
            self.slide
                .dataset
                .scenes
                .get(req.scene.get())
                .ok_or(WsiError::SceneOutOfRange {
                    index: req.scene.get(),
                    count: self.slide.dataset.scenes.len(),
                })?;
        let series = scene
            .series
            .get(req.series.get())
            .ok_or(WsiError::SeriesOutOfRange {
                index: req.series.get(),
                count: scene.series.len(),
            })?;
        let level_meta =
            series
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: series.levels.len() as u32,
                })?;
        let level =
            self.slide
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                })?;

        let TileLayout::Regular {
            tiles_across,
            tiles_down,
            ..
        } = level_meta.tile_layout
        else {
            return Err(WsiError::UnsupportedFormat(
                "VMS levels must use regular tiles".into(),
            ));
        };

        if req.col < 0
            || req.row < 0
            || req.col >= tiles_across as i64
            || req.row >= tiles_down as i64
        {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!(
                    "tile ({},{}) out of range ({}x{})",
                    req.col, req.row, tiles_across, tiles_down
                ),
            });
        }

        let base_tiles_across = level.base_tiles_across;
        let base_tiles_down = level.base_tiles_down;
        let jpeg_col = req.col as u32 / base_tiles_across;
        let jpeg_row = req.row as u32 / base_tiles_down;
        let local_tile_col = req.col as u32 % base_tiles_across;
        let local_tile_row = req.row as u32 % base_tiles_down;
        let jpeg = level
            .jpegs
            .get((jpeg_row * level.jpegs_across + jpeg_col) as usize)
            .ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "VMS tile resolved to missing JPEG shard".into(),
            })?;
        if local_tile_col >= jpeg.tiles_across || local_tile_row >= jpeg.tiles_down {
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "VMS local tile coordinates out of JPEG shard bounds".into(),
            });
        }
        let tile_index = (local_tile_row * jpeg.tiles_across + local_tile_col) as usize;
        jpeg.decode_tile(tile_index, level.scale_denom, backend)
            .map_err(|err| match err {
                WsiError::TileRead { .. } => err,
                other => WsiError::TileRead {
                    col: req.col,
                    row: req.row,
                    level: req.level.get(),
                    reason: other.to_string(),
                },
            })
    }
}

fn vms_image_count(columns: u32, rows: u32) -> Option<usize> {
    u64::from(columns)
        .checked_mul(u64::from(rows))
        .filter(|count| *count <= MAX_VMS_JPEG_SHARDS)
        .and_then(|count| usize::try_from(count).ok())
}

#[cfg(test)]
#[path = "slide/tests/limits.rs"]
mod limit_tests;
