use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use byteorder::{LittleEndian, ReadBytesExt};
use j2k_core::BackendRequest;

use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::limits::{
    checked_product_to_usize, MAX_COMPRESSED_INPUT_BYTES, MAX_DECODED_IMAGE_BYTES,
};
use crate::core::registry::{
    read_cpu_tiles_with_backend, DatasetReader, FormatProbe, ProbeConfidence, ProbeResult,
    SlideReader,
};
use crate::core::types::*;
use crate::decode::jp2k::{decode_batch_jp2k, Jp2kDecodeJob};
use crate::error::WsiError;
use crate::properties::Properties;

const OLYMPUS_JPEG_2000: u32 = 3;
const ETS_BACKGROUND_BYTES: u64 = 40;
const MAX_ETS_DIMENSIONS: u32 = 16;
const MAX_ETS_TILES: u32 = 1_000_000;
const MAX_ETS_LEVEL_INDEX: u32 = 1_023;
const MAX_ETS_AXIS_INDEX: u32 = 65_535;
const MAX_ETS_SCENES: usize = 1_024;

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

impl DatasetReader for OlympusVsiBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        Ok(Box::new(OlympusVsiReader {
            slide: Arc::new(OlympusVsiSlide::parse(path)?),
        }))
    }
}

struct OlympusVsiReader {
    slide: Arc<OlympusVsiSlide>,
}

impl SlideReader for OlympusVsiReader {
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
            "RequireDevice not supported for Olympus VSI",
            |req, backend| self.read_tile_with_backend(req, backend),
        )
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.read_tile_with_backend(req, BackendRequest::Auto)
    }
}

impl OlympusVsiReader {
    fn read_tile_with_backend(
        &self,
        req: &TileRequest,
        backend: BackendRequest,
    ) -> Result<CpuTile, WsiError> {
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

        let key = EtsTileKey {
            level: req.level.get(),
            z: req.plane.get().z,
            c: req.plane.get().c,
            t: req.plane.get().t,
            col: req.col as u32,
            row: req.row as u32,
        };
        let Some(tile) = scene.tiles.get(&key) else {
            return scene.background_tile(level.tile_width, level.tile_height);
        };
        scene
            .decode_tile(tile, backend)
            .map_err(|err| WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: err.to_string(),
            })
    }
}

struct OlympusVsiSlide {
    dataset: Dataset,
    scenes: Vec<EtsScene>,
}

impl OlympusVsiSlide {
    fn parse(path: &Path) -> Result<Self, WsiError> {
        let dir =
            companion_dir(path).ok_or_else(|| invalid_slide(path, "missing companion dir"))?;
        let mut ets_paths = find_ets_files(&dir)?;
        if ets_paths.is_empty() {
            return Err(invalid_slide(path, "no ETS frame files found"));
        }

        let mut scenes = ets_paths
            .drain(..)
            .map(|ets_path| EtsScene::parse(&ets_path))
            .collect::<Result<Vec<_>, _>>()?;
        scenes.sort_by_key(|scene| Reverse(scene.level0_area()));

        let mut quickhash = Quickhash1::new();
        quickhash.hash_string(&path.display().to_string());
        for scene in &scenes {
            quickhash.hash_string(&scene.path.display().to_string());
            quickhash.update(&scene.path.metadata()?.len().to_le_bytes());
        }
        let quickhash = quickhash
            .finish()
            .ok_or_else(|| invalid_slide(path, "failed to compute Olympus quickhash"))?;
        let dataset_id = dataset_id_from_quickhash(path, &quickhash, "quickhash")?;

        let public_scenes = scenes
            .iter()
            .enumerate()
            .map(|(scene_index, scene)| Scene {
                id: format!("s{scene_index}"),
                name: scene.name.clone(),
                series: vec![Series {
                    id: "ser0".into(),
                    axes: scene.axes,
                    levels: scene
                        .levels
                        .iter()
                        .map(|level| Level {
                            dimensions: (level.width as u64, level.height as u64),
                            downsample: scene.levels[0].width as f64 / level.width as f64,
                            tile_layout: TileLayout::Regular {
                                tile_width: level.tile_width,
                                tile_height: level.tile_height,
                                tiles_across: level.tiles_across as u64,
                                tiles_down: level.tiles_down as u64,
                            },
                        })
                        .collect(),
                    sample_type: scene.sample_type,
                    channels: scene.channels.clone(),
                }],
            })
            .collect();

        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "olympus");
        properties.insert("openslide.quickhash-1", quickhash);

        Ok(Self {
            dataset: Dataset {
                id: dataset_id,
                scenes: public_scenes,
                associated_images: HashMap::new(),
                properties,
                icc_profiles: HashMap::new(),
                source_icc_profiles: Vec::new(),
            },
            scenes,
        })
    }
}

struct EtsScene {
    path: PathBuf,
    name: Option<String>,
    levels: Vec<EtsLevel>,
    tiles: HashMap<EtsTileKey, EtsTile>,
    axes: AxesShape,
    sample_type: SampleType,
    samples_per_pixel: u32,
    background: Vec<u8>,
    channels: Vec<ChannelInfo>,
}

impl EtsScene {
    fn parse(path: &Path) -> Result<Self, WsiError> {
        let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
        let file_len = file.metadata()?.len();
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;
        if !fourcc_matches(&magic, b"SIS") {
            return Err(invalid_slide(path, "invalid ETS SIS magic"));
        }
        let _header_size = file.read_u32::<LittleEndian>()?;
        let _version = file.read_u32::<LittleEndian>()?;
        let n_dimensions = file.read_u32::<LittleEndian>()?;
        let additional_header_offset = file.read_u64::<LittleEndian>()?;
        let _additional_header_size = file.read_u32::<LittleEndian>()?;
        file.seek(SeekFrom::Current(4))?;
        let used_chunk_offset = file.read_u64::<LittleEndian>()?;
        let n_used_chunks = file.read_u32::<LittleEndian>()?;
        file.seek(SeekFrom::Current(4))?;

        file.seek(SeekFrom::Start(additional_header_offset))?;
        file.read_exact(&mut magic)?;
        if !fourcc_matches(&magic, b"ETS") {
            return Err(invalid_slide(path, "invalid ETS header magic"));
        }
        file.seek(SeekFrom::Current(4))?;
        let pixel_type = file.read_u32::<LittleEndian>()?;
        let samples_per_pixel = file.read_u32::<LittleEndian>()?;
        let _colorspace = file.read_u32::<LittleEndian>()?;
        let compression = file.read_u32::<LittleEndian>()?;
        let _compression_quality = file.read_u32::<LittleEndian>()?;
        let tile_width = file.read_u32::<LittleEndian>()?;
        let tile_height = file.read_u32::<LittleEndian>()?;
        let _tile_z = file.read_u32::<LittleEndian>()?;
        file.seek(SeekFrom::Current(4 * 17))?;

        let sample_type = sample_type_from_ets(pixel_type)?;
        let background_len = validate_ets_header_limits(
            n_dimensions,
            n_used_chunks,
            samples_per_pixel,
            sample_type.byte_size(),
            tile_width,
            tile_height,
        )
        .map_err(|message| invalid_slide(path, message))?;
        let mut background = vec![0; background_len];
        file.read_exact(&mut background)?;
        let remaining_background = ETS_BACKGROUND_BYTES as usize - background_len;
        file.seek(SeekFrom::Current(remaining_background as i64))?;
        let _component_order = file.read_u32::<LittleEndian>()?;
        let use_pyramid = file.read_u32::<LittleEndian>()? != 0;

        if compression != OLYMPUS_JPEG_2000 {
            return Err(invalid_slide(
                path,
                format!("unsupported ETS compression {compression}"),
            ));
        }
        validate_ets_chunk_table(file_len, used_chunk_offset, n_dimensions, n_used_chunks)
            .map_err(|message| invalid_slide(path, message))?;

        file.seek(SeekFrom::Start(used_chunk_offset))?;
        let tile_index_limit = u64::from(MAX_ETS_TILES)
            * u64::try_from(std::mem::size_of::<(EtsTileKey, EtsTile)>()).unwrap_or(u64::MAX);
        let tile_index_bytes = u64::from(n_used_chunks)
            * u64::try_from(std::mem::size_of::<(EtsTileKey, EtsTile)>()).unwrap_or(u64::MAX);
        let mut tiles = HashMap::new();
        tiles
            .try_reserve(n_used_chunks as usize)
            .map_err(|_| WsiError::ResourceLimit {
                resource: "Olympus ETS tile index",
                requested: tile_index_bytes,
                limit: tile_index_limit,
            })?;
        let mut max_level = 0u32;
        let mut max_z = 0u32;
        let mut max_c = 0u32;
        let mut max_t = 0u32;
        for _ in 0..n_used_chunks {
            file.seek(SeekFrom::Current(4))?;
            let mut coords = Vec::with_capacity(n_dimensions as usize);
            for _ in 0..n_dimensions {
                coords.push(file.read_i32::<LittleEndian>()?);
            }
            let offset = file.read_u64::<LittleEndian>()?;
            let byte_count = file.read_u32::<LittleEndian>()?;
            file.seek(SeekFrom::Current(4))?;

            let key = key_from_coords(&coords, use_pyramid)?;
            checked_ets_level_count(key.level).map_err(|message| invalid_slide(path, message))?;
            for (name, value) in [("z", key.z), ("c", key.c), ("t", key.t)] {
                checked_ets_axis_len(value, name)
                    .map_err(|message| invalid_slide(path, message))?;
            }
            if byte_count == 0 {
                return Err(invalid_slide(path, "ETS tile payload is empty"));
            }
            let tile_end = offset
                .checked_add(u64::from(byte_count))
                .ok_or_else(|| invalid_slide(path, "ETS tile payload range overflows"))?;
            if tile_end > file_len {
                return Err(invalid_slide(
                    path,
                    format!(
                        "ETS tile payload range {offset}..{tile_end} exceeds file length {file_len}"
                    ),
                ));
            }
            max_level = max_level.max(key.level);
            max_z = max_z.max(key.z);
            max_c = max_c.max(key.c);
            max_t = max_t.max(key.t);
            if tiles.insert(key, EtsTile { offset, byte_count }).is_some() {
                return Err(invalid_slide(path, "duplicate ETS tile coordinates"));
            }
        }

        let level_count =
            checked_ets_level_count(max_level).map_err(|message| invalid_slide(path, message))?;
        let mut max_col_by_level = vec![0u32; level_count];
        let mut max_row_by_level = vec![0u32; level_count];
        for key in tiles.keys() {
            let idx = key.level as usize;
            max_col_by_level[idx] = max_col_by_level[idx].max(key.col);
            max_row_by_level[idx] = max_row_by_level[idx].max(key.row);
        }

        let levels = max_col_by_level
            .into_iter()
            .zip(max_row_by_level)
            .map(|(max_col, max_row)| {
                Ok(EtsLevel {
                    width: checked_ets_extent(max_col, tile_width, "width")?,
                    height: checked_ets_extent(max_row, tile_height, "height")?,
                    tile_width,
                    tile_height,
                    tiles_across: max_col + 1,
                    tiles_down: max_row + 1,
                })
            })
            .collect::<Result<Vec<_>, String>>()
            .map_err(|message| invalid_slide(path, message))?;

        let axes = AxesShape {
            z: checked_ets_axis_len(max_z, "z").map_err(|message| invalid_slide(path, message))?,
            c: checked_ets_axis_len(max_c, "c").map_err(|message| invalid_slide(path, message))?,
            t: checked_ets_axis_len(max_t, "t").map_err(|message| invalid_slide(path, message))?,
        };

        let channels = if samples_per_pixel == 3 {
            Vec::new()
        } else {
            let channel_bytes = u64::from(axes.c)
                * u64::try_from(std::mem::size_of::<ChannelInfo>()).unwrap_or(u64::MAX);
            let channel_limit = u64::from(MAX_ETS_AXIS_INDEX + 1)
                * u64::try_from(std::mem::size_of::<ChannelInfo>()).unwrap_or(u64::MAX);
            let mut channels = Vec::new();
            channels
                .try_reserve_exact(axes.c as usize)
                .map_err(|_| WsiError::ResourceLimit {
                    resource: "Olympus ETS channel metadata",
                    requested: channel_bytes,
                    limit: channel_limit,
                })?;
            channels.extend((0..axes.c).map(|c| ChannelInfo {
                name: Some(format!("Channel {c}")),
                color: None,
                excitation_nm: None,
                emission_nm: None,
            }));
            channels
        };

        Ok(Self {
            path: path.to_path_buf(),
            name: path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .map(ToOwned::to_owned),
            levels,
            tiles,
            axes,
            sample_type,
            samples_per_pixel,
            background,
            channels,
        })
    }

    fn level0_area(&self) -> u64 {
        self.levels
            .first()
            .map(|level| level.width as u64 * level.height as u64)
            .unwrap_or(0)
    }

    fn decode_tile(&self, tile: &EtsTile, backend: BackendRequest) -> Result<CpuTile, WsiError> {
        let mut file = File::open(&self.path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: self.path.clone(),
        })?;
        file.seek(SeekFrom::Start(tile.offset))?;
        let encoded_len = checked_product_to_usize(
            &[u64::from(tile.byte_count)],
            MAX_COMPRESSED_INPUT_BYTES,
            "Olympus ETS tile payload",
        )
        .map_err(WsiError::DisplayConversion)?;
        let mut bytes = vec![0; encoded_len];
        file.read_exact(&mut bytes)?;
        crate::core::batch::exactly_one(
            decode_batch_jp2k(&[Jp2kDecodeJob {
                data: Cow::Owned(bytes),
                expected_width: self.levels[0].tile_width,
                expected_height: self.levels[0].tile_height,
                rgb_color_space: true,
                backend,
            }]),
            "Olympus ETS JP2K decode",
        )?
    }

    fn background_tile(&self, width: u32, height: u32) -> Result<CpuTile, WsiError> {
        let byte_len = checked_product_to_usize(
            &[u64::from(width), u64::from(height), 3],
            MAX_DECODED_IMAGE_BYTES,
            "Olympus background tile",
        )
        .map_err(WsiError::DisplayConversion)?;
        let pixel_count = checked_product_to_usize(
            &[u64::from(width), u64::from(height)],
            MAX_DECODED_IMAGE_BYTES,
            "Olympus background pixel count",
        )
        .map_err(WsiError::DisplayConversion)?;
        let mut bytes = Vec::with_capacity(byte_len);
        let rgb = if self.samples_per_pixel >= 3 && self.background.len() >= 3 {
            [self.background[0], self.background[1], self.background[2]]
        } else {
            let gray = self.background.first().copied().unwrap_or(0);
            [gray, gray, gray]
        };
        for _ in 0..pixel_count {
            bytes.extend_from_slice(&rgb);
        }
        CpuTile::new(
            width,
            height,
            3,
            ColorSpace::Rgb,
            CpuTileLayout::Interleaved,
            CpuTileData::u8(bytes),
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct EtsLevel {
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    tiles_across: u32,
    tiles_down: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct EtsTileKey {
    level: u32,
    z: u32,
    c: u32,
    t: u32,
    col: u32,
    row: u32,
}

#[derive(Clone, Copy, Debug)]
struct EtsTile {
    offset: u64,
    byte_count: u32,
}

fn validate_ets_header_limits(
    n_dimensions: u32,
    n_used_chunks: u32,
    samples_per_pixel: u32,
    sample_bytes: usize,
    tile_width: u32,
    tile_height: u32,
) -> Result<usize, String> {
    if !(3..=MAX_ETS_DIMENSIONS).contains(&n_dimensions) {
        return Err(format!(
            "ETS coordinate dimensionality {n_dimensions} is outside the supported range 3..={MAX_ETS_DIMENSIONS}"
        ));
    }
    if !(1..=MAX_ETS_TILES).contains(&n_used_chunks) {
        return Err(format!(
            "ETS tile count {n_used_chunks} is outside the supported range 1..={MAX_ETS_TILES}"
        ));
    }
    if samples_per_pixel == 0 {
        return Err("ETS samples per pixel must be nonzero".into());
    }
    if tile_width == 0 || tile_height == 0 {
        return Err(format!(
            "ETS tile dimensions must be nonzero, got {tile_width}x{tile_height}"
        ));
    }
    checked_product_to_usize(
        &[
            u64::from(samples_per_pixel),
            u64::try_from(sample_bytes).unwrap_or(u64::MAX),
        ],
        ETS_BACKGROUND_BYTES,
        "Olympus ETS background",
    )
}

fn validate_ets_chunk_table(
    file_len: u64,
    offset: u64,
    n_dimensions: u32,
    n_used_chunks: u32,
) -> Result<(), String> {
    let entry_bytes = u64::from(n_dimensions)
        .checked_mul(4)
        .and_then(|coordinate_bytes| coordinate_bytes.checked_add(20))
        .ok_or_else(|| "ETS chunk-table entry size overflows".to_string())?;
    let table_bytes = entry_bytes
        .checked_mul(u64::from(n_used_chunks))
        .ok_or_else(|| "ETS chunk-table size overflows".to_string())?;
    let table_end = offset
        .checked_add(table_bytes)
        .ok_or_else(|| "ETS chunk-table range overflows".to_string())?;
    if table_end > file_len {
        return Err(format!(
            "ETS chunk-table range {offset}..{table_end} exceeds file length {file_len}"
        ));
    }
    Ok(())
}

fn checked_ets_level_count(max_level: u32) -> Result<usize, String> {
    if max_level > MAX_ETS_LEVEL_INDEX {
        return Err(format!(
            "ETS level index {max_level} exceeds the supported maximum {MAX_ETS_LEVEL_INDEX}"
        ));
    }
    usize::try_from(max_level + 1)
        .map_err(|_| "ETS level count is not addressable on this platform".into())
}

fn checked_ets_axis_len(max_index: u32, name: &str) -> Result<u32, String> {
    if max_index > MAX_ETS_AXIS_INDEX {
        return Err(format!(
            "ETS {name} index {max_index} exceeds the supported maximum {MAX_ETS_AXIS_INDEX}"
        ));
    }
    max_index
        .checked_add(1)
        .ok_or_else(|| format!("ETS {name} axis length overflows"))
}

fn checked_ets_extent(max_tile_index: u32, tile_size: u32, name: &str) -> Result<u32, String> {
    max_tile_index
        .checked_add(1)
        .and_then(|tile_count| tile_count.checked_mul(tile_size))
        .ok_or_else(|| format!("ETS {name} overflows 32-bit dimensions"))
}

fn key_from_coords(coords: &[i32], use_pyramid: bool) -> Result<EtsTileKey, WsiError> {
    if coords.len() < 3 {
        return Err(invalid_slide(
            Path::new(""),
            "ETS coordinate dimensionality is too small",
        ));
    }
    let upper = if use_pyramid {
        coords.len().saturating_sub(1)
    } else {
        coords.len()
    };
    let level = if use_pyramid {
        checked_coord(coords[coords.len() - 1], "resolution")?
    } else {
        0
    };
    let extra = &coords[2..upper];
    let z = extra
        .first()
        .copied()
        .map(|value| checked_coord(value, "z"))
        .transpose()?
        .unwrap_or(0);
    let c = extra
        .get(1)
        .copied()
        .map(|value| checked_coord(value, "c"))
        .transpose()?
        .unwrap_or(0);
    let t = extra
        .get(2)
        .copied()
        .map(|value| checked_coord(value, "t"))
        .transpose()?
        .unwrap_or(0);
    Ok(EtsTileKey {
        level,
        z,
        c,
        t,
        col: checked_coord(coords[0], "x")?,
        row: checked_coord(coords[1], "y")?,
    })
}

fn checked_coord(value: i32, name: &str) -> Result<u32, WsiError> {
    u32::try_from(value).map_err(|_| WsiError::InvalidSlide {
        path: PathBuf::new(),
        message: format!("negative ETS {name} coordinate {value}"),
    })
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

fn sample_type_from_ets(pixel_type: u32) -> Result<SampleType, WsiError> {
    match pixel_type {
        1 | 2 => Ok(SampleType::Uint8),
        3 | 4 => Ok(SampleType::Uint16),
        9 => Ok(SampleType::Float32),
        other => Err(WsiError::UnsupportedFormat(format!(
            "unsupported ETS pixel type {other}"
        ))),
    }
}

fn find_ets_files(dir: &Path) -> Result<Vec<PathBuf>, WsiError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: dir.to_path_buf(),
    })? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let frame = path.join("frame_t.ets");
        if frame.is_file() {
            if paths.len() == MAX_ETS_SCENES {
                return Err(invalid_slide(
                    dir,
                    format!("Olympus dataset exceeds the {MAX_ETS_SCENES}-scene limit"),
                ));
            }
            paths.push(frame);
        }
    }
    paths.sort();
    Ok(paths)
}

fn companion_dir(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    Some(parent.join(format!("_{stem}_")))
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

fn fourcc_matches(bytes: &[u8; 4], tag: &[u8; 3]) -> bool {
    &bytes[..3] == tag && (bytes[3] == 0 || bytes[3] == b' ')
}

#[cfg(test)]
mod tests;
