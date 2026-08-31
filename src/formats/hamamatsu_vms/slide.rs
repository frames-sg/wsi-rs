use super::model::{VmsJpeg, VmsLevel, VmsSlide};
use super::*;

const MAX_VMS_JPEG_SHARDS: u64 = 65_536;

mod parse;

pub(super) struct VmsReader {
    pub(super) slide: Arc<VmsSlide>,
}

struct ResolvedVmsTile<'a> {
    level: &'a VmsLevel,
    jpeg: &'a VmsJpeg,
    tile_index: usize,
    width: u32,
    height: u32,
}

impl SlideReader for VmsReader {
    fn dataset(&self) -> &Dataset {
        &self.slide.dataset
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        match self.resolve_tile(req) {
            Ok(tile) if tile.level.scale_denom == 1 => TileCodecKind::Jpeg,
            Ok(_) | Err(_) => TileCodecKind::Other,
        }
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        read_cpu_tiles(reqs, |req, backend| {
            self.read_tile_with_backend(req, backend)
        })
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Cpu)
    }

    fn read_raw_compressed_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        self.read_raw_jpeg_tile(req).map_err(|error| match error {
            WsiError::TileRead { .. } | WsiError::Unsupported { .. } => error,
            other => WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: other.to_string(),
            },
        })
    }

    fn read_raw_compressed_display_tile(
        &self,
        req: &TileViewRequest,
    ) -> Result<RawCompressedTile, WsiError> {
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
        let level =
            series
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: series.levels.len() as u32,
                })?;
        let TileLayout::Regular {
            tile_width,
            tile_height,
            ..
        } = level.tile_layout
        else {
            return Err(WsiError::Unsupported {
                reason: "VMS raw JPEG display access requires a regular native tile grid".into(),
            });
        };
        if (req.tile_width, req.tile_height) != (tile_width, tile_height) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "VMS raw JPEG display tile size {}x{} does not match native tile size {}x{}",
                    req.tile_width, req.tile_height, tile_width, tile_height
                ),
            });
        }

        self.read_raw_compressed_tile(&TileRequest {
            scene: req.scene,
            series: req.series,
            level: req.level,
            plane: req.plane,
            col: req.col,
            row: req.row,
        })
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
        let data = read_file_bounded(path, self.slide.encoded_unit_bytes, "VMS associated JPEG")
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
            let retained_bytes = u64::try_from(tile.data.byte_size()).unwrap_or(u64::MAX);
            self.slide
                .associated_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .put(name.to_string(), tile.clone(), retained_bytes);
            tile.as_ref().clone()
        })
    }
}

impl VmsReader {
    fn resolve_tile(&self, req: &TileRequest) -> Result<ResolvedVmsTile<'_>, WsiError> {
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

        let jpeg_col = req.col as u32 / level.base_tiles_across;
        let jpeg_row = req.row as u32 / level.base_tiles_down;
        let local_tile_col = req.col as u32 % level.base_tiles_across;
        let local_tile_row = req.row as u32 % level.base_tiles_down;
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
        let width = jpeg
            .tile_width
            .min(jpeg.width.saturating_sub(local_tile_col * jpeg.tile_width));
        let height = jpeg.tile_height.min(
            jpeg.height
                .saturating_sub(local_tile_row * jpeg.tile_height),
        );
        Ok(ResolvedVmsTile {
            level,
            jpeg: jpeg.as_ref(),
            tile_index,
            width,
            height,
        })
    }

    fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let tile = self.resolve_tile(req)?;
        tile.jpeg
            .decode_tile(tile.tile_index, tile.level.scale_denom, backend)
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

    fn read_raw_jpeg_tile(&self, req: &TileRequest) -> Result<RawCompressedTile, WsiError> {
        let tile = self.resolve_tile(req)?;
        if tile.level.scale_denom != 1 {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "VMS raw JPEG access requires a full-resolution physical tile; level {} is scaled by {}",
                    req.level.get(),
                    tile.level.scale_denom
                ),
            });
        }

        let data = tile
            .jpeg
            .tile_jpeg_bytes(tile.tile_index, tile.width, tile.height)?;
        let raw = crate::decode::jpeg::standalone_raw_jpeg_tile(data)?;
        if (raw.width(), raw.height()) != (tile.width, tile.height) {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "VMS raw JPEG dimensions {}x{} do not match logical tile {}x{}",
                    raw.width(),
                    raw.height(),
                    tile.width,
                    tile.height
                ),
            });
        }
        Ok(raw)
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
