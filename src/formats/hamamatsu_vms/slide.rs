use super::model::{VmsJpeg, VmsLevel, VmsSlide};
use super::*;

const MAX_VMS_JPEG_SHARDS: u64 = 65_536;
const ASSOCIATED_FILE_CHANGED: &str = "VMS associated image changed after the slide was opened";

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
            other => WsiError::tile_read(req, other.to_string()),
        })
    }

    fn read_raw_compressed_display_tile(
        &self,
        req: &TileViewRequest,
    ) -> Result<RawCompressedTile, WsiError> {
        let tile = crate::formats::geometry::native_regular_display_tile(
            &self.slide.dataset,
            req,
            "VMS raw JPEG display",
        )?;
        self.read_raw_compressed_tile(&tile)
    }

    fn associated_encoding(&self, name: &str) -> Result<Option<SourceEncoding>, WsiError> {
        // VMS associated images are standalone JPEG files.
        let decoded = self
            .dataset()
            .associated_images
            .get(name)
            .and_then(|image| image.decoded_bytes());
        let (Some(file), Some(decoded)) = (self.slide.associated_files.get(name), decoded) else {
            return Ok(None);
        };
        // Sized from the identity recorded at open, once the file still
        // matches it; a replaced file is refused as in read_associated.
        file.identity
            .ensure_matches(&file.path, ASSOCIATED_FILE_CHANGED)?;
        Ok(Some(
            SourceEncoding::new(Compression::Jpeg).with_sizes(decoded, file.identity.length()),
        ))
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
        let file = self
            .slide
            .associated_files
            .get(name)
            .ok_or_else(|| WsiError::AssociatedImageNotFound(name.into()))?;
        let source =
            FileIdentity::open_verified(&file.path, &file.identity, ASSOCIATED_FILE_CHANGED)?;
        let data =
            read_to_end_bounded(source, self.slide.encoded_unit_bytes, "VMS associated JPEG")
                .map_err(|source| WsiError::io_with_path(&file.path, source))?;
        crate::decode::jpeg::decode_standalone_jpeg(&data).map(|tile| {
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
        let (_, _, level_meta) = self.slide.dataset.level(req.scene, req.series, req.level)?;
        let level =
            self.slide
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                })?;

        if level_meta.regular_tile(req)?.is_none() {
            return Err(WsiError::UnsupportedFormat(
                "VMS levels must use regular tiles".into(),
            ));
        }

        let jpeg_col = req.col as u32 / level.base_tiles_across;
        let jpeg_row = req.row as u32 / level.base_tiles_down;
        let local_tile_col = req.col as u32 % level.base_tiles_across;
        let local_tile_row = req.row as u32 % level.base_tiles_down;
        let jpeg = level
            .jpegs
            .get((jpeg_row * level.jpegs_across + jpeg_col) as usize)
            .ok_or_else(|| WsiError::tile_read(req, "VMS tile resolved to missing JPEG shard"))?;
        if local_tile_col >= jpeg.tiles_across || local_tile_row >= jpeg.tiles_down {
            return Err(WsiError::tile_read(
                req,
                "VMS local tile coordinates out of JPEG shard bounds",
            ));
        }
        let tile_index = (local_tile_row * jpeg.tiles_across + local_tile_col) as usize;
        let (width, height) = jpeg.tile_size(local_tile_col, local_tile_row);
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
                other => WsiError::tile_read(req, other.to_string()),
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
