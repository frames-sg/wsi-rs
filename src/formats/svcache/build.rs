use super::storage::{fingerprint_source, read_svcache, svcache_matches_source};
use super::*;

/// The opened source slide, its fingerprint and a scratch payload file, with
/// the output directory created.
struct BuildSource {
    slide: Slide,
    fingerprint: SourceFingerprint,
    payload: File,
}

impl BuildSource {
    fn open(source_path: &Path, out_path: &Path) -> Result<Self, WsiError> {
        let registry = FormatRegistry::builtin_native();
        let source = registry.open(source_path)?;
        let slide = Slide::from_source_with_cache_bytes(source, 256 * 1024 * 1024);
        let fingerprint = fingerprint_source(source_path)?;
        let parent = out_path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        Ok(Self {
            slide,
            fingerprint,
            payload: tempfile::tempfile()?,
        })
    }
}

pub fn build_svcache(source_path: &Path, out_path: &Path) -> Result<(), WsiError> {
    let BuildSource {
        slide,
        fingerprint,
        mut payload,
    } = BuildSource::open(source_path, out_path)?;
    let mut scenes = metadata_shell(slide.dataset())?;
    for ((scene_idx, series_idx, level_idx), level) in indexed_levels_mut(&mut scenes) {
        level.tiles = Vec::with_capacity(
            usize::try_from(level.tiles_across.saturating_mul(level.tiles_down)).unwrap_or(0),
        );
        for row in 0..level.tiles_down {
            for col in 0..level.tiles_across {
                let request = TileViewRequest {
                    scene: scene_idx.into(),
                    series: series_idx.into(),
                    level: (level_idx as u32).into(),
                    plane: PlaneSelection::default().into(),
                    col: i64::try_from(col).unwrap_or(i64::MAX),
                    row: i64::try_from(row).unwrap_or(i64::MAX),
                    tile_width: level.tile_width,
                    tile_height: level.tile_height,
                };
                let tile = slide.read_display_tile(&request)?;
                level
                    .tiles
                    .push(Some(write_tile_payload(&mut payload, &tile)?));
            }
        }
    }

    let associated = build_associated_payloads(&slide, &mut payload)?;
    let metadata = SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete: true,
        source: fingerprint,
        properties: dataset_properties(slide.dataset()),
        scenes,
        associated,
    };
    write_svcache_file(out_path, &metadata, payload)
}

pub fn build_svcache_tiles(
    source_path: &Path,
    out_path: &Path,
    selections: &[SvcacheTileSelection],
) -> Result<usize, WsiError> {
    build_svcache_tiles_with_existing_policy(
        source_path,
        out_path,
        selections,
        ExistingTilePolicy::Preserve,
    )
}

pub fn build_svcache_tiles_replace(
    source_path: &Path,
    out_path: &Path,
    selections: &[SvcacheTileSelection],
) -> Result<usize, WsiError> {
    build_svcache_tiles_with_existing_policy(
        source_path,
        out_path,
        selections,
        ExistingTilePolicy::Replace,
    )
}

pub fn build_svcache_tile_payloads_replace(
    source_path: &Path,
    out_path: &Path,
    tiles: &[(SvcacheTileSelection, CpuTile)],
) -> Result<usize, WsiError> {
    build_svcache_tile_payloads_with_existing_policy(
        source_path,
        out_path,
        tiles,
        ExistingTilePolicy::Replace,
    )
}

pub fn build_svcache_tile_payloads_merge(
    source_path: &Path,
    out_path: &Path,
    tiles: &[(SvcacheTileSelection, CpuTile)],
) -> Result<usize, WsiError> {
    build_svcache_tile_payloads_with_existing_policy(
        source_path,
        out_path,
        tiles,
        ExistingTilePolicy::Preserve,
    )
}

fn build_svcache_tile_payloads_with_existing_policy(
    source_path: &Path,
    out_path: &Path,
    tiles: &[(SvcacheTileSelection, CpuTile)],
    existing_tile_policy: ExistingTilePolicy,
) -> Result<usize, WsiError> {
    let inputs = tiles
        .iter()
        .map(|(selection, tile)| PartialTileInput::Provided {
            selection: *selection,
            tile,
        })
        .collect();
    build_partial_svcache_with_existing_policy(source_path, out_path, inputs, existing_tile_policy)
}

fn build_svcache_tiles_with_existing_policy(
    source_path: &Path,
    out_path: &Path,
    selections: &[SvcacheTileSelection],
    existing_tile_policy: ExistingTilePolicy,
) -> Result<usize, WsiError> {
    let inputs = selections
        .iter()
        .copied()
        .map(PartialTileInput::ReadFromSource)
        .collect();
    build_partial_svcache_with_existing_policy(source_path, out_path, inputs, existing_tile_policy)
}

enum PartialTileInput<'a> {
    ReadFromSource(SvcacheTileSelection),
    Provided {
        selection: SvcacheTileSelection,
        tile: &'a CpuTile,
    },
}

impl PartialTileInput<'_> {
    fn selection(&self) -> SvcacheTileSelection {
        match self {
            Self::ReadFromSource(selection) => *selection,
            Self::Provided { selection, .. } => *selection,
        }
    }

    fn write_payload(
        &self,
        slide: &Slide,
        payload: &mut File,
        tile_width: u32,
        tile_height: u32,
    ) -> Result<TileMeta, WsiError> {
        match self {
            Self::ReadFromSource(selection) => {
                let request = TileViewRequest {
                    scene: selection.scene,
                    series: selection.series,
                    level: selection.level,
                    plane: selection.plane,
                    col: selection.col,
                    row: selection.row,
                    tile_width,
                    tile_height,
                };
                let tile = slide.read_display_tile(&request)?;
                write_tile_payload(payload, &tile)
            }
            Self::Provided { tile, .. } => write_tile_payload(payload, tile),
        }
    }
}

struct PartialTileSlot {
    index: u64,
    scene_idx: usize,
    series_idx: usize,
    level_idx: usize,
}

fn build_partial_svcache_with_existing_policy(
    source_path: &Path,
    out_path: &Path,
    inputs: Vec<PartialTileInput<'_>>,
    existing_tile_policy: ExistingTilePolicy,
) -> Result<usize, WsiError> {
    let BuildSource {
        slide,
        fingerprint,
        mut payload,
    } = BuildSource::open(source_path, out_path)?;
    let mut metadata = partial_svcache_metadata(
        fingerprint,
        slide.dataset(),
        metadata_shell(slide.dataset())?,
    );
    let _copied = copy_existing_svcache_tiles_with_policy(
        out_path,
        source_path,
        &mut metadata.scenes,
        &mut payload,
        existing_tile_policy,
    )?;

    let mut written = 0usize;
    for input in unique_partial_tile_inputs(inputs) {
        let slot = partial_tile_slot(&metadata, input.selection())?;
        let level =
            &mut metadata.scenes[slot.scene_idx].series[slot.series_idx].levels[slot.level_idx];
        if level.tile_meta_for_index(slot.index).is_some() {
            continue;
        }
        let tile_meta =
            input.write_payload(&slide, &mut payload, level.tile_width, level.tile_height)?;
        level.insert_tile_for_index(slot.index, tile_meta);
        written += 1;
    }

    write_svcache_file(out_path, &metadata, payload)?;
    Ok(written)
}

fn unique_partial_tile_inputs(mut inputs: Vec<PartialTileInput<'_>>) -> Vec<PartialTileInput<'_>> {
    inputs.sort_by_key(|input| partial_tile_selection_sort_key(input.selection()));
    inputs.dedup_by_key(|input| input.selection());
    inputs
}

fn partial_tile_selection_sort_key(
    selection: SvcacheTileSelection,
) -> (usize, usize, u32, u32, u32, u32, i64, i64) {
    let plane = selection.plane.get();
    (
        selection.scene.get(),
        selection.series.get(),
        selection.level.get(),
        plane.z,
        plane.c,
        plane.t,
        selection.row,
        selection.col,
    )
}

fn partial_tile_slot(
    metadata: &SvcacheMetadata,
    selection: SvcacheTileSelection,
) -> Result<PartialTileSlot, WsiError> {
    let level = metadata.level(
        selection.scene.get(),
        selection.series.get(),
        selection.level.get(),
    )?;
    let index = level.tile_index(selection.col, selection.row, selection.level.get())?;
    Ok(PartialTileSlot {
        index,
        scene_idx: selection.scene.get(),
        series_idx: selection.series.get(),
        level_idx: selection.level.get() as usize,
    })
}

fn partial_svcache_metadata(
    source: SourceFingerprint,
    dataset: &Dataset,
    scenes: Vec<SceneMeta>,
) -> SvcacheMetadata {
    SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete: false,
        source,
        properties: dataset_properties(dataset),
        scenes,
        associated: Vec::new(),
    }
}

fn dataset_properties(dataset: &Dataset) -> Vec<(String, String)> {
    dataset
        .properties
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn scene_metadata(scene: &Scene, series: Vec<SeriesMeta>) -> SceneMeta {
    SceneMeta {
        id: scene.id.clone(),
        name: scene.name.clone(),
        series,
    }
}

fn series_metadata(series: &Series, levels: Vec<LevelMeta>) -> SeriesMeta {
    SeriesMeta {
        id: series.id.clone(),
        axes: AxesMeta {
            z: series.axes.z,
            c: series.axes.c,
            t: series.axes.t,
        },
        sample_type: SampleTypeMeta::Uint8,
        channels: series
            .channels
            .iter()
            .map(|channel| ChannelMeta {
                name: channel.name.clone(),
                color: channel.color,
            })
            .collect(),
        levels,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExistingTilePolicy {
    Preserve,
    Replace,
}

pub(super) fn copy_existing_svcache_tiles_with_policy(
    out_path: &Path,
    source_path: &Path,
    scenes: &mut [SceneMeta],
    payload: &mut File,
    policy: ExistingTilePolicy,
) -> Result<usize, WsiError> {
    match policy {
        ExistingTilePolicy::Preserve => {
            copy_existing_svcache_tiles(out_path, source_path, scenes, payload)
        }
        ExistingTilePolicy::Replace => Ok(0),
    }
}

pub(super) fn copy_existing_svcache_tiles(
    out_path: &Path,
    source_path: &Path,
    scenes: &mut [SceneMeta],
    payload: &mut File,
) -> Result<usize, WsiError> {
    if !out_path.is_file() || !svcache_matches_source(out_path, source_path)? {
        return Ok(0);
    }
    let (mut existing_file, payload_start, existing_metadata) = read_svcache(out_path)?;
    let mut copied = 0usize;

    for ((scene_idx, series_idx, level_idx), level) in indexed_levels_mut(scenes) {
        let Some(existing_level) = existing_metadata
            .scenes
            .get(scene_idx)
            .and_then(|scene| scene.series.get(series_idx))
            .and_then(|series| series.levels.get(level_idx))
        else {
            continue;
        };
        // A tile index names the same tile only on the same grid.
        if !existing_level.has_tile_grid_of(level) {
            continue;
        }
        let tile_limit = level
            .tiles_across
            .checked_mul(level.tiles_down)
            .ok_or_else(|| invalid_svcache(out_path, "svcache level tile count overflow"))?;
        for (idx, existing_tile) in existing_level.populated_tiles() {
            // Validation already bounds every index by the shared grid.
            if idx >= tile_limit {
                return Err(invalid_svcache(out_path, "svcache tile index out of range"));
            }
            if level.tile_meta_for_index(idx).is_some() {
                continue;
            }
            let copied_tile = copy_tile_payload(
                out_path,
                &mut existing_file,
                payload_start,
                existing_tile,
                payload,
            )?;
            level.insert_tile_for_index(idx, copied_tile);
            copied += 1;
        }
    }

    Ok(copied)
}

/// Every level with its scene, series and level indexes.
fn indexed_levels_mut(
    scenes: &mut [SceneMeta],
) -> impl Iterator<Item = ((usize, usize, usize), &mut LevelMeta)> {
    scenes
        .iter_mut()
        .enumerate()
        .flat_map(|(scene_idx, scene)| {
            scene
                .series
                .iter_mut()
                .enumerate()
                .flat_map(move |(series_idx, series)| {
                    series
                        .levels
                        .iter_mut()
                        .enumerate()
                        .map(move |(level_idx, level)| ((scene_idx, series_idx, level_idx), level))
                })
        })
}

fn copy_tile_payload(
    cache_path: &Path,
    existing_file: &mut File,
    payload_start: u64,
    existing_tile: &TileMeta,
    payload: &mut File,
) -> Result<TileMeta, WsiError> {
    let source_offset = payload_start
        .checked_add(existing_tile.payload_offset)
        .ok_or_else(|| invalid_svcache(cache_path, "svcache payload offset overflow"))?;
    existing_file.seek(SeekFrom::Start(source_offset))?;
    let payload_offset = payload.seek(SeekFrom::End(0))?;
    let mut limited = existing_file.take(existing_tile.payload_len);
    let copied_len = std::io::copy(&mut limited, payload)?;
    if copied_len != existing_tile.payload_len {
        return Err(invalid_svcache(
            cache_path,
            format!(
                "svcache payload copy was truncated: expected {}, copied {copied_len}",
                existing_tile.payload_len
            ),
        ));
    }
    let mut copied = existing_tile.clone();
    copied.payload_offset = payload_offset;
    Ok(copied)
}

pub(super) fn metadata_shell(dataset: &Dataset) -> Result<Vec<SceneMeta>, WsiError> {
    let mut scenes = Vec::with_capacity(dataset.scenes.len());
    for scene in &dataset.scenes {
        let mut series_meta = Vec::with_capacity(scene.series.len());
        for series in &scene.series {
            let mut levels_meta = Vec::with_capacity(series.levels.len());
            for level in &series.levels {
                let (tile_width, tile_height, tiles_across, tiles_down) =
                    cache_grid_for_level(level);
                levels_meta.push(LevelMeta {
                    dimensions: level.dimensions,
                    downsample: level.downsample,
                    tile_width,
                    tile_height,
                    tiles_across,
                    tiles_down,
                    tiles: Vec::new(),
                    sparse_tiles: Vec::new(),
                });
            }
            series_meta.push(series_metadata(series, levels_meta));
        }
        scenes.push(scene_metadata(scene, series_meta));
    }
    Ok(scenes)
}

pub(super) fn cache_grid_for_level(level: &Level) -> (u32, u32, u64, u64) {
    match &level.tile_layout {
        TileLayout::Regular {
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        } => (*tile_width, *tile_height, *tiles_across, *tiles_down),
        TileLayout::WholeLevel { width, height, .. } => (
            DEFAULT_TILE_SIZE,
            DEFAULT_TILE_SIZE,
            width.div_ceil(u64::from(DEFAULT_TILE_SIZE)),
            height.div_ceil(u64::from(DEFAULT_TILE_SIZE)),
        ),
        TileLayout::Irregular { .. } => {
            let width = level.dimensions.0;
            let height = level.dimensions.1;
            (
                DEFAULT_TILE_SIZE,
                DEFAULT_TILE_SIZE,
                width.div_ceil(u64::from(DEFAULT_TILE_SIZE)),
                height.div_ceil(u64::from(DEFAULT_TILE_SIZE)),
            )
        }
    }
}

pub(super) fn write_tile_payload(file: &mut File, tile: &CpuTile) -> Result<TileMeta, WsiError> {
    let raw = match (tile.layout, &tile.data) {
        (CpuTileLayout::Interleaved, CpuTileData::U8(samples)) => samples.as_slice(),
        _ => {
            return Err(WsiError::UnsupportedFormat(
                ".svcache builder only supports interleaved uint8 display tiles".into(),
            ));
        }
    };
    let color_space = ColorSpaceMeta::try_from(&tile.color_space)?;
    let encoded = crate::core::compression::encode_zstd(raw).map_err(|err| WsiError::Codec {
        codec: "svcache-zstd",
        source: Box::new(err),
    })?;
    let payload_offset = file.stream_position()?;
    file.write_all(&encoded)?;
    Ok(TileMeta {
        payload_offset,
        payload_len: encoded.len() as u64,
        decoded_len: raw.len(),
        width: tile.width,
        height: tile.height,
        channels: tile.channels,
        color_space,
        codec: PayloadCodec::Zstd,
        sha256: format!("{:x}", Sha256::digest(&encoded)),
    })
}

fn build_associated_payloads(
    slide: &Slide,
    payload: &mut File,
) -> Result<Vec<AssociatedMeta>, WsiError> {
    let mut associated = Vec::new();
    let mut names = slide
        .dataset()
        .associated_images
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    if names.is_empty() {
        names.extend(
            ["thumbnail", "macro", "label"]
                .into_iter()
                .map(str::to_string),
        );
    }
    names.sort();
    names.dedup();
    for name in names {
        match slide.read_associated(&name) {
            Ok(tile) => associated.push(AssociatedMeta {
                name,
                dimensions: (tile.width, tile.height),
                tile: write_tile_payload(payload, &tile)?,
            }),
            Err(WsiError::AssociatedImageNotFound(_)) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(associated)
}

pub(super) fn write_svcache_file(
    out_path: &Path,
    metadata: &SvcacheMetadata,
    mut payload: File,
) -> Result<(), WsiError> {
    let metadata_json = serde_json::to_vec(metadata)
        .map_err(|err| invalid_svcache(out_path, format!("serialize svcache metadata: {err}")))?;
    let parent = out_path.parent().unwrap_or_else(|| Path::new("."));
    let mut out = tempfile::NamedTempFile::new_in(parent)?;
    out.write_all(MAGIC)?;
    out.write_all(&(metadata_json.len() as u64).to_le_bytes())?;
    out.write_all(&metadata_json)?;
    payload.seek(SeekFrom::Start(0))?;
    std::io::copy(&mut payload, &mut out)?;
    out.flush()?;
    out.persist(out_path)
        .map_err(|err| WsiError::Io(err.error))?;
    Ok(())
}
