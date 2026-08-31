mod helpers;
mod index;
mod slide;

#[cfg(test)]
mod tests;

use helpers::invalid_slide;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::cache::{CacheConfig, PrivateCache};
use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::registry::{
    crop_rgb_interleaved_u8_buffer, read_cpu_tiles, BackendOpenConfig, ConfiguredDatasetReader,
    ConfiguredFormatProbe, ConservativeManagedReader, DatasetReader, FormatProbe,
    ManagedSlideReader, OpenBudget, ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::*;
use crate::decode::jpeg::jpeg_dimensions;
use crate::decode::jpeg::{decode_batch_jpeg, JpegDecodeJob};
use crate::error::WsiError;
use crate::formats::companion_path::resolve_companion_file;
use crate::formats::ini::ParsedIni;
use crate::properties::Properties;
use flate2::read::ZlibDecoder;
use j2k_core::BackendRequest;

const MRXS_EXT: &str = "mrxs";
const SLIDEDAT_INI: &str = "Slidedat.ini";
const INDEX_VERSION: &str = "01.02";
const SLIDEDAT_MAX_SIZE: u64 = 1 << 20;
const KEY_FILE_MAX_SIZE: u64 = 1 << 20;
const SLIDE_POSITION_RECORD_SIZE: usize = 9;
const MIRAX_ASSOCIATED_DIMENSION_PROBE_BYTES: u64 = 64 << 10;
const MIRAX_QUICKHASH_READ_BUFFER_BYTES: usize = 64 << 10;
const GROUP_GENERAL: &str = "GENERAL";
const KEY_SLIDE_ID: &str = "SLIDE_ID";
const KEY_IMAGE_NUMBER_X: &str = "IMAGENUMBER_X";
const KEY_IMAGE_NUMBER_Y: &str = "IMAGENUMBER_Y";
const KEY_OBJECTIVE_MAGNIFICATION: &str = "OBJECTIVE_MAGNIFICATION";
const KEY_CAMERA_IMAGE_DIVISIONS_PER_SIDE: &str = "CameraImageDivisionsPerSide";

const GROUP_HIERARCHICAL: &str = "HIERARCHICAL";
const KEY_HIER_COUNT: &str = "HIER_COUNT";
const KEY_NONHIER_COUNT: &str = "NONHIER_COUNT";
const KEY_INDEXFILE: &str = "INDEXFILE";
const KEY_HIER_NAME: &str = "HIER_%d_NAME";
const KEY_HIER_COUNT_FMT: &str = "HIER_%d_COUNT";
const KEY_HIER_VAL_SECTION_FMT: &str = "HIER_%d_VAL_%d_SECTION";
const KEY_NONHIER_NAME: &str = "NONHIER_%d_NAME";
const KEY_NONHIER_COUNT_FMT: &str = "NONHIER_%d_COUNT";
const KEY_NONHIER_VAL_FMT: &str = "NONHIER_%d_VAL_%d";
const KEY_NONHIER_VAL_SECTION_FMT: &str = "NONHIER_%d_VAL_%d_SECTION";
const KEY_MACRO_IMAGE_TYPE: &str = "THUMBNAIL_IMAGE_TYPE";
const KEY_LABEL_IMAGE_TYPE: &str = "BARCODE_IMAGE_TYPE";
const KEY_THUMBNAIL_IMAGE_TYPE: &str = "PREVIEW_IMAGE_TYPE";
const VALUE_VIMSLIDE_POSITION_BUFFER: &str = "VIMSLIDE_POSITION_BUFFER";
const VALUE_STITCHING_INTENSITY_LAYER: &str = "StitchingIntensityLayer";
const VALUE_SCAN_DATA_LAYER: &str = "Scan data layer";
const VALUE_SCAN_DATA_LAYER_MACRO: &str = "ScanDataLayer_SlideThumbnail";
const VALUE_SCAN_DATA_LAYER_LABEL: &str = "ScanDataLayer_SlideBarcode";
const VALUE_SCAN_DATA_LAYER_THUMBNAIL: &str = "ScanDataLayer_SlidePreview";
const VALUE_SLIDE_ZOOM_LEVEL: &str = "Slide zoom level";

const GROUP_DATAFILE: &str = "DATAFILE";
const KEY_FILE_COUNT: &str = "FILE_COUNT";
const KEY_FILE_FMT: &str = "FILE_%d";

const KEY_OVERLAP_X: &str = "OVERLAP_X";
const KEY_OVERLAP_Y: &str = "OVERLAP_Y";
const KEY_MPP_X: &str = "MICROMETER_PER_PIXEL_X";
const KEY_MPP_Y: &str = "MICROMETER_PER_PIXEL_Y";
const KEY_IMAGE_FORMAT: &str = "IMAGE_FORMAT";
const KEY_IMAGE_FILL_COLOR_BGR: &str = "IMAGE_FILL_COLOR_BGR";
const KEY_DIGITIZER_WIDTH: &str = "DIGITIZER_WIDTH";
const KEY_DIGITIZER_HEIGHT: &str = "DIGITIZER_HEIGHT";
const KEY_IMAGE_CONCAT_FACTOR: &str = "IMAGE_CONCAT_FACTOR";

#[cfg(test)]
static MIRAX_ASSOCIATED_CACHE_HITS: AtomicU64 = AtomicU64::new(0);

pub(crate) struct MiraxBackend;

impl MiraxBackend {
    pub(crate) fn new() -> Self {
        Self
    }

    fn parse_with_cache_config(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Arc<MiraxSlide>, WsiError> {
        Ok(Arc::new(MiraxSlide::parse_with_cache_config(
            path,
            cache_config,
        )?))
    }

    fn parse_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Arc<MiraxSlide>, WsiError> {
        Ok(Arc::new(MiraxSlide::parse_with_config(path, config)?))
    }

    fn open_parsed(
        &self,
        path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        let slide = self.parse_with_cache_config(path, cache_config)?;
        Ok(Box::new(MiraxReader { slide }))
    }

    fn open_parsed_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        let slide = self.parse_with_config(path, config)?;
        Ok(Box::new(MiraxReader { slide }))
    }
}

impl FormatProbe for MiraxBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        self.probe_with_config(path, BackendOpenConfig::deterministic())
    }
}

impl ConfiguredFormatProbe for MiraxBackend {
    fn probe_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<ProbeResult, WsiError> {
        if !looks_like_mirax(path) {
            return Ok(ProbeResult::not_detected(""));
        }
        let _ = config;
        Ok(ProbeResult::detected("mirax", ProbeConfidence::Definite))
    }
}

impl DatasetReader for MiraxBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        self.open_parsed(path, CacheConfig::deterministic())
    }
}

impl ConfiguredDatasetReader for MiraxBackend {
    fn open_with_config(
        &self,
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Box<dyn ManagedSlideReader>, WsiError> {
        Ok(Box::new(ConservativeManagedReader::new(
            self.open_parsed_with_config(path, config)?,
            config.limits.encoded_unit_bytes(),
        )))
    }
}

struct MiraxReader {
    slide: Arc<MiraxSlide>,
}

impl SlideReader for MiraxReader {
    fn dataset(&self) -> &Dataset {
        &self.slide.dataset
    }

    fn tile_codec_kind(&self, req: &TileRequest) -> TileCodecKind {
        match self.tile_for_request(req) {
            Ok((_, tile)) if tile.image.format == MiraxImageFormat::Jpeg => TileCodecKind::Jpeg,
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
        let (entry, tile) = self.tile_for_request(req)?;
        if tile.image.format != MiraxImageFormat::Jpeg {
            return Err(WsiError::Unsupported {
                reason: "MIRAX raw compressed tile access requires a JPEG backing image".into(),
            });
        }
        if tile.src_x != 0 || tile.src_y != 0 {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "MIRAX raw JPEG passthrough cannot represent a logical tile cropped from source offset ({}, {})",
                    tile.src_x, tile.src_y
                ),
            });
        }

        let data = self.slide.read_record_bytes(&tile.image.record)?;
        let raw = crate::decode::jpeg::standalone_raw_jpeg_tile(data)?;
        if (raw.width(), raw.height()) != entry.dimensions {
            return Err(WsiError::Unsupported {
                reason: format!(
                    "MIRAX raw JPEG geometry {}x{} does not exactly match logical tile {}x{}",
                    raw.width(),
                    raw.height(),
                    entry.dimensions.0,
                    entry.dimensions.1
                ),
            });
        }
        Ok(raw)
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.slide.read_associated(name)
    }
}

impl MiraxReader {
    fn tile_for_request<'a>(
        &'a self,
        req: &TileRequest,
    ) -> Result<(&'a TileEntry, &'a MiraxTile), WsiError> {
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
        validate_mirax_plane(req.plane.get(), series.axes)?;
        let TileLayout::Irregular { tiles, .. } = &level.tile_layout else {
            return Err(WsiError::UnsupportedFormat(
                "MIRAX levels must use irregular tiles".into(),
            ));
        };
        let entry = tiles
            .get(&(req.col, req.row))
            .ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!("no MIRAX tile at ({},{})", req.col, req.row),
            })?;
        let tile_index = entry.tiff_tile_index.ok_or_else(|| WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "MIRAX tile is missing backing descriptor".into(),
        })?;
        let level_state =
            self.slide
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: self.slide.levels.len() as u32,
                })?;
        let tile = level_state
            .tiles
            .get(tile_index)
            .ok_or_else(|| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: format!("invalid MIRAX tile descriptor index {tile_index}"),
            })?;
        Ok((entry, tile))
    }

    fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let (entry, tile) = self.tile_for_request(req)?;
        let decoded = self.slide.decode_image_with_backend(&tile.image, backend)?;
        if tile.src_x == 0
            && tile.src_y == 0
            && decoded.width == entry.dimensions.0
            && decoded.height == entry.dimensions.1
        {
            return Ok(Arc::unwrap_or_clone(decoded));
        }
        crop_rgb_interleaved_u8_buffer(
            decoded.as_ref(),
            tile.src_x,
            tile.src_y,
            entry.dimensions.0,
            entry.dimensions.1,
        )
    }
}

fn validate_mirax_plane(plane: PlaneSelection, axes: AxesShape) -> Result<(), WsiError> {
    for (axis, value, extent) in [
        ("z", plane.z, axes.z),
        ("c", plane.c, axes.c),
        ("t", plane.t, axes.t),
    ] {
        if value >= extent {
            return Err(WsiError::PlaneOutOfRange {
                axis: axis.into(),
                value,
                max: extent.saturating_sub(1),
            });
        }
    }
    Ok(())
}

struct MiraxSlide {
    dataset: Dataset,
    levels: Vec<MiraxLevel>,
    associated: HashMap<String, MiraxRecord>,
    decoded_images: Mutex<PrivateCache<u32, Arc<CpuTile>>>,
    associated_cache: Mutex<PrivateCache<String, Arc<CpuTile>>>,
    open_files: Mutex<HashMap<PathBuf, File>>,
    encoded_unit_bytes: u64,
}

struct MiraxLevel {
    tiles: Vec<MiraxTile>,
}

struct MiraxLevelBuilder {
    dimensions: (u64, u64),
    downsample: f64,
    image_format: MiraxImageFormat,
    raw_image_width: u32,
    raw_image_height: u32,
    tile_width: f64,
    tile_height: f64,
    tile_advance_x: f64,
    tile_advance_y: f64,
    tiles: HashMap<(i64, i64), TileEntry>,
    descriptors: Vec<MiraxTile>,
    extra_tiles: (u32, u32, u32, u32),
}

#[derive(Clone)]
struct MiraxTile {
    image: Arc<MiraxImage>,
    src_x: u32,
    src_y: u32,
}

#[derive(Clone)]
struct MiraxImage {
    id: u32,
    record: MiraxRecord,
    format: MiraxImageFormat,
    expected_width: u32,
    expected_height: u32,
}

#[derive(Clone)]
struct MiraxRecord {
    path: PathBuf,
    offset: u64,
    len: u64,
}

#[derive(Clone, Copy)]
struct SlideZoomLevelSection {
    concat_exponent: i32,
    overlap_x: f64,
    overlap_y: f64,
    mpp_x: f64,
    mpp_y: f64,
    fill_rgb: u32,
    image_format: MiraxImageFormat,
    image_w: u32,
    image_h: u32,
}

#[derive(Clone, Copy)]
struct SlideZoomLevelParams {
    image_concat: u32,
    tile_count_divisor: u32,
    tiles_per_image: u32,
    positions_per_tile: u32,
    tile_advance_x: f64,
    tile_advance_y: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MiraxImageFormat {
    Jpeg,
    Png,
    Bmp24,
}

fn looks_like_mirax(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case(MRXS_EXT))
        .unwrap_or(false)
        && slide_dir_from_entry(path)
            .ok()
            .map(|dir| dir.join(SLIDEDAT_INI).is_file())
            .unwrap_or(false)
}

fn slide_dir_from_entry(path: &Path) -> Result<PathBuf, WsiError> {
    if !path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case(MRXS_EXT))
        .unwrap_or(false)
    {
        return Err(WsiError::UnsupportedFormat(path.display().to_string()));
    }
    let stem = path
        .file_stem()
        .ok_or_else(|| invalid_slide(path, "MIRAX entry has no stem"))?;
    let dir = path.with_file_name(stem);
    if !dir.is_dir() {
        return Err(invalid_slide(
            path,
            format!("missing MIRAX directory {}", dir.display()),
        ));
    }
    Ok(dir)
}
