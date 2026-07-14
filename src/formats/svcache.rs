mod build;
mod paths;
mod reader;
mod storage;

#[cfg(test)]
mod tests;

pub use build::{
    build_svcache, build_svcache_tile_payloads_merge, build_svcache_tile_payloads_replace,
    build_svcache_tiles, build_svcache_tiles_replace,
};
pub(crate) use paths::resolve_open_path_with_policy;
pub use paths::{cache_dir_svcache_path, default_svcache_path, svcache_candidate_paths};
pub use storage::svcache_matches_source;

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::registry::{
    DatasetReader, FormatProbe, FormatRegistry, ProbeConfidence, ProbeResult, Slide, SlideReader,
};
use crate::core::types::{
    AssociatedImage, AxesShape, ChannelInfo, ColorSpace, CpuTile, CpuTileLayout, Dataset,
    DatasetId, Level, LevelIdx, PlaneIdx, PlaneSelection, SampleType, Scene, SceneId, Series,
    SeriesId, TileLayout, TileOutputPreference, TilePixels, TileRequest, TileViewRequest,
};
use crate::error::WsiError;
use crate::properties::Properties;

const MAGIC: &[u8; 8] = b"SVCACHE1";
const SCHEMA_VERSION: u32 = 3;
const DEFAULT_TILE_SIZE: u32 = 256;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SvcachePolicy {
    #[default]
    Off,
    PreferFresh,
    RequireFresh,
}

impl SvcachePolicy {
    pub fn from_env_value(value: Option<&str>) -> Self {
        match value.unwrap_or("off").trim().to_ascii_lowercase().as_str() {
            "prefer" | "on" | "true" | "1" => Self::PreferFresh,
            "required" | "require" => Self::RequireFresh,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SvcacheMetadata {
    schema_version: u32,
    #[serde(default = "default_cache_complete")]
    complete: bool,
    source: SourceFingerprint,
    properties: Vec<(String, String)>,
    scenes: Vec<SceneMeta>,
    associated: Vec<AssociatedMeta>,
}

fn default_cache_complete() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceFingerprint {
    path: String,
    len: u64,
    modified_unix_nanos: Option<u128>,
    sample_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SceneMeta {
    id: String,
    name: Option<String>,
    series: Vec<SeriesMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SeriesMeta {
    id: String,
    axes: AxesMeta,
    sample_type: SampleTypeMeta,
    channels: Vec<ChannelMeta>,
    levels: Vec<LevelMeta>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct AxesMeta {
    z: u32,
    c: u32,
    t: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum SampleTypeMeta {
    Uint8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChannelMeta {
    name: Option<String>,
    color: Option<[u8; 3]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LevelMeta {
    dimensions: (u64, u64),
    downsample: f64,
    tile_width: u32,
    tile_height: u32,
    tiles_across: u64,
    tiles_down: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tiles: Vec<Option<TileMeta>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sparse_tiles: Vec<SparseTileMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SparseTileMeta {
    index: u64,
    tile: TileMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AssociatedMeta {
    name: String,
    dimensions: (u32, u32),
    tile: TileMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TileMeta {
    payload_offset: u64,
    payload_len: u64,
    decoded_len: usize,
    width: u32,
    height: u32,
    channels: u16,
    color_space: ColorSpaceMeta,
    codec: PayloadCodec,
    sha256: String,
}

impl LevelMeta {
    fn tile_meta_for_index(&self, index: u64) -> Option<&TileMeta> {
        if !self.tiles.is_empty() {
            return usize::try_from(index)
                .ok()
                .and_then(|idx| self.tiles.get(idx))
                .and_then(Option::as_ref);
        }
        self.sparse_tiles
            .binary_search_by_key(&index, |entry| entry.index)
            .ok()
            .map(|idx| &self.sparse_tiles[idx].tile)
    }

    fn insert_tile_for_index(&mut self, index: u64, tile: TileMeta) {
        if !self.tiles.is_empty() {
            if let Ok(idx) = usize::try_from(index) {
                if let Some(slot) = self.tiles.get_mut(idx) {
                    *slot = Some(tile);
                }
            }
            return;
        }

        match self
            .sparse_tiles
            .binary_search_by_key(&index, |entry| entry.index)
        {
            Ok(idx) => self.sparse_tiles[idx].tile = tile,
            Err(idx) => self
                .sparse_tiles
                .insert(idx, SparseTileMeta { index, tile }),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum PayloadCodec {
    Zstd,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum ColorSpaceMeta {
    Rgb,
    Rgba,
    Grayscale,
}

pub struct SvcacheBackend;

pub struct SvcacheReader {
    file: Mutex<File>,
    payload_start: u64,
    metadata: SvcacheMetadata,
    dataset: Dataset,
    associated_index: HashMap<String, usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SvcacheTileSelection {
    pub scene: SceneId,
    pub series: SeriesId,
    pub level: LevelIdx,
    pub plane: PlaneIdx,
    pub col: i64,
    pub row: i64,
}

impl SvcacheTileSelection {
    pub fn new(
        scene: impl Into<SceneId>,
        series: impl Into<SeriesId>,
        level: impl Into<LevelIdx>,
        col: i64,
        row: i64,
    ) -> Self {
        Self {
            scene: scene.into(),
            series: series.into(),
            level: level.into(),
            plane: PlaneIdx::default(),
            col,
            row,
        }
    }

    pub fn with_plane(mut self, plane: impl Into<PlaneIdx>) -> Self {
        self.plane = plane.into();
        self
    }
}
