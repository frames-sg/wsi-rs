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
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use flate2::read::ZlibDecoder;
use j2k_core::BackendRequest;
use lru::LruCache;

use crate::core::file_identity::FileIdentity;
use crate::core::hash::Quickhash1;
use crate::core::registry::{
    crop_rgb_interleaved_u8_buffer, read_cpu_tiles_with_backend, DatasetReader, FormatProbe,
    ProbeConfidence, ProbeResult, SlideReader,
};
use crate::core::types::*;
use crate::decode::jpeg::jpeg_dimensions;
use crate::decode::jpeg::{decode_batch_jpeg, JpegDecodeJob};
use crate::error::WsiError;
use crate::formats::companion_path::resolve_companion_file;
use crate::formats::ini::ParsedIni;
use crate::properties::Properties;

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

pub(crate) struct MiraxBackend {
    probe_cache: Mutex<LruCache<FileIdentity, Arc<MiraxSlide>>>,
}

impl MiraxBackend {
    pub(crate) fn new() -> Self {
        Self {
            probe_cache: Mutex::new(LruCache::new(NonZeroUsize::new(16).unwrap())),
        }
    }

    fn parse(&self, path: &Path) -> Result<Arc<MiraxSlide>, WsiError> {
        Ok(Arc::new(MiraxSlide::parse(path)?))
    }
}

impl Default for MiraxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatProbe for MiraxBackend {
    fn probe(&self, path: &Path) -> Result<ProbeResult, WsiError> {
        if !looks_like_mirax(path) {
            return Ok(not_detected());
        }
        let key = FileIdentity::from_path(path)?;
        if self
            .probe_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .is_some()
        {
            return Ok(ProbeResult {
                detected: true,
                vendor: "mirax".into(),
                confidence: ProbeConfidence::Definite,
            });
        }
        let slide = match self.parse(path) {
            Ok(slide) => slide,
            Err(_) => return Ok(not_detected()),
        };
        self.probe_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(key, slide);
        Ok(ProbeResult {
            detected: true,
            vendor: "mirax".into(),
            confidence: ProbeConfidence::Definite,
        })
    }
}

impl DatasetReader for MiraxBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        let key = FileIdentity::from_path(path)?;
        let cached = self
            .probe_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop(&key);
        let slide = match cached {
            Some(slide) => slide,
            None => self.parse(path)?,
        };
        Ok(Box::new(MiraxReader { slide }))
    }
}

struct MiraxReader {
    slide: Arc<MiraxSlide>,
}

impl SlideReader for MiraxReader {
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
            "RequireDevice not supported for MIRAX in Phase 2",
            |req, backend| self.read_tile_with_backend(req, backend),
        )
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Auto)
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.slide.read_associated(name)
    }
}

impl MiraxReader {
    fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
        let series = &self.slide.dataset.scenes[req.scene.get()].series[req.series.get()];
        let level =
            series
                .levels
                .get(req.level.get() as usize)
                .ok_or(WsiError::LevelOutOfRange {
                    level: req.level.get(),
                    count: series.levels.len() as u32,
                })?;
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

struct MiraxSlide {
    dataset: Dataset,
    levels: Vec<MiraxLevel>,
    associated: HashMap<String, MiraxRecord>,
    decoded_images: Mutex<LruCache<u32, Arc<CpuTile>>>,
    associated_cache: Mutex<LruCache<String, Arc<CpuTile>>>,
    open_files: Mutex<HashMap<PathBuf, File>>,
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

fn not_detected() -> ProbeResult {
    ProbeResult {
        detected: false,
        vendor: String::new(),
        confidence: ProbeConfidence::Likely,
    }
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
