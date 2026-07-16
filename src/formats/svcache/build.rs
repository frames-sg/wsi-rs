use super::storage::{fingerprint_source, hex_encode, read_svcache, svcache_matches_source};
use super::*;

pub fn build_svcache(source_path: &Path, out_path: &Path) -> Result<(), WsiError> {
    let registry = FormatRegistry::builtin_native();
    let source = registry.open(source_path)?;
    let slide = Slide::from_source_with_cache_bytes(source, 256 * 1024 * 1024);
    let source_fingerprint = fingerprint_source(source_path)?;

    let parent = out_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut payload = tempfile::tempfile()?;
    let mut scenes = Vec::new();

    for (scene_idx, scene) in slide.dataset().scenes.iter().enumerate() {
        let mut series_meta = Vec::new();
        for (series_idx, series) in scene.series.iter().enumerate() {
            let mut levels_meta = Vec::new();
            for (level_idx, level) in series.levels.iter().enumerate() {
                let (tile_width, tile_height, tiles_across, tiles_down) =
                    cache_grid_for_level(level);
                let mut tiles = Vec::with_capacity(
                    usize::try_from(tiles_across.saturating_mul(tiles_down)).unwrap_or(0),
                );
                for row in 0..tiles_down {
                    for col in 0..tiles_across {
                        let request = TileViewRequest {
                            scene: scene_idx.into(),
                            series: series_idx.into(),
                            level: (level_idx as u32).into(),
                            plane: PlaneSelection::default().into(),
                            col: i64::try_from(col).unwrap_or(i64::MAX),
                            row: i64::try_from(row).unwrap_or(i64::MAX),
                            tile_width,
                            tile_height,
                        };
                        let tile = slide.read_display_tile(&request)?;
                        tiles.push(Some(write_tile_payload(&mut payload, &tile)?));
                    }
                }
                levels_meta.push(LevelMeta {
                    dimensions: level.dimensions,
                    downsample: level.downsample,
                    tile_width,
                    tile_height,
                    tiles_across,
                    tiles_down,
                    tiles,
                    sparse_tiles: Vec::new(),
                });
            }
            series_meta.push(series_metadata(series, levels_meta));
        }
        scenes.push(scene_metadata(scene, series_meta));
    }

    let associated = build_associated_payloads(&slide, &mut payload)?;
    let metadata = SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete: true,
        source: source_fingerprint,
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
    tile_width: u32,
    tile_height: u32,
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
    let registry = FormatRegistry::builtin_native();
    let source = registry.open(source_path)?;
    let slide = Slide::from_source_with_cache_bytes(source, 256 * 1024 * 1024);
    let source_fingerprint = fingerprint_source(source_path)?;

    let parent = out_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut payload = tempfile::tempfile()?;
    let mut scenes = metadata_shell(slide.dataset())?;
    let _copied = copy_existing_svcache_tiles_with_policy(
        out_path,
        source_path,
        &mut scenes,
        &mut payload,
        existing_tile_policy,
    )?;

    let mut written = 0usize;
    for input in unique_partial_tile_inputs(inputs) {
        let selection = input.selection();
        let slot = partial_tile_slot(slide.dataset(), selection)?;
        if scenes[slot.scene_idx].series[slot.series_idx].levels[slot.level_idx]
            .tile_meta_for_index(slot.index)
            .is_some()
        {
            continue;
        }
        let tile_meta =
            input.write_payload(&slide, &mut payload, slot.tile_width, slot.tile_height)?;
        scenes[slot.scene_idx].series[slot.series_idx].levels[slot.level_idx]
            .insert_tile_for_index(slot.index, tile_meta);
        written += 1;
    }

    let metadata = partial_svcache_metadata(source_fingerprint, slide.dataset(), scenes);
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
    dataset: &Dataset,
    selection: SvcacheTileSelection,
) -> Result<PartialTileSlot, WsiError> {
    let (tile_width, tile_height, tiles_across, tiles_down) =
        level_grid_for_selection(dataset, selection)?;
    if selection.col < 0 || selection.row < 0 {
        return Err(WsiError::TileRead {
            col: selection.col,
            row: selection.row,
            level: selection.level.get(),
            reason: ".svcache selection has negative tile coordinate".into(),
        });
    }
    let col = selection.col as u64;
    let row = selection.row as u64;
    if col >= tiles_across || row >= tiles_down {
        return Err(WsiError::TileRead {
            col: selection.col,
            row: selection.row,
            level: selection.level.get(),
            reason: ".svcache selection tile coordinate out of range".into(),
        });
    }
    let index = row
        .checked_mul(tiles_across)
        .and_then(|base| base.checked_add(col))
        .ok_or_else(|| WsiError::TileRead {
            col: selection.col,
            row: selection.row,
            level: selection.level.get(),
            reason: ".svcache selection tile index overflow".into(),
        })?;

    Ok(PartialTileSlot {
        tile_width,
        tile_height,
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

    for (scene_idx, scene) in scenes.iter_mut().enumerate() {
        let Some(existing_scene) = existing_metadata.scenes.get(scene_idx) else {
            continue;
        };
        for (series_idx, series) in scene.series.iter_mut().enumerate() {
            let Some(existing_series) = existing_scene.series.get(series_idx) else {
                continue;
            };
            for (level_idx, level) in series.levels.iter_mut().enumerate() {
                let Some(existing_level) = existing_series.levels.get(level_idx) else {
                    continue;
                };
                let tile_limit = level
                    .tiles_across
                    .checked_mul(level.tiles_down)
                    .ok_or_else(|| WsiError::InvalidSlide {
                        path: out_path.into(),
                        message: "svcache level tile count overflow".into(),
                    })?;
                for (idx, existing_slot) in existing_level.tiles.iter().enumerate() {
                    let idx = u64::try_from(idx).map_err(|_| WsiError::InvalidSlide {
                        path: out_path.into(),
                        message: "svcache tile index overflow".into(),
                    })?;
                    if idx >= tile_limit {
                        break;
                    }
                    if level.tile_meta_for_index(idx).is_some() {
                        continue;
                    }
                    let Some(existing_tile) = existing_slot else {
                        continue;
                    };
                    let copied_tile = copy_tile_payload(
                        &mut existing_file,
                        payload_start,
                        existing_tile,
                        payload,
                    )?;
                    level.insert_tile_for_index(idx, copied_tile);
                    copied += 1;
                }
                for existing_entry in &existing_level.sparse_tiles {
                    if existing_entry.index >= tile_limit {
                        return Err(WsiError::InvalidSlide {
                            path: out_path.into(),
                            message: "svcache sparse tile index out of range".into(),
                        });
                    }
                    if level.tile_meta_for_index(existing_entry.index).is_some() {
                        continue;
                    }
                    let copied_tile = copy_tile_payload(
                        &mut existing_file,
                        payload_start,
                        &existing_entry.tile,
                        payload,
                    )?;
                    level.insert_tile_for_index(existing_entry.index, copied_tile);
                    copied += 1;
                }
            }
        }
    }

    Ok(copied)
}

fn copy_tile_payload(
    existing_file: &mut File,
    payload_start: u64,
    existing_tile: &TileMeta,
    payload: &mut File,
) -> Result<TileMeta, WsiError> {
    let source_offset = payload_start
        .checked_add(existing_tile.payload_offset)
        .ok_or_else(|| WsiError::InvalidSlide {
            path: PathBuf::from(".svcache"),
            message: "svcache payload offset overflow".into(),
        })?;
    existing_file.seek(SeekFrom::Start(source_offset))?;
    let payload_offset = payload.seek(SeekFrom::End(0))?;
    let mut limited = existing_file.take(existing_tile.payload_len);
    let copied_len = std::io::copy(&mut limited, payload)?;
    if copied_len != existing_tile.payload_len {
        return Err(WsiError::InvalidSlide {
            path: PathBuf::from(".svcache"),
            message: format!(
                "svcache payload copy was truncated: expected {}, copied {copied_len}",
                existing_tile.payload_len
            ),
        });
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

pub(super) fn level_grid_for_selection(
    dataset: &Dataset,
    selection: SvcacheTileSelection,
) -> Result<(u32, u32, u64, u64), WsiError> {
    let level = dataset
        .scenes
        .get(selection.scene.get())
        .and_then(|scene| scene.series.get(selection.series.get()))
        .and_then(|series| series.levels.get(selection.level.get() as usize))
        .ok_or_else(|| WsiError::LevelOutOfRange {
            level: selection.level.get(),
            count: 0,
        })?;
    Ok(cache_grid_for_level(level))
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
    if tile.layout != CpuTileLayout::Interleaved || tile.data.sample_type() != SampleType::Uint8 {
        return Err(WsiError::UnsupportedFormat(
            ".svcache builder only supports interleaved uint8 display tiles".into(),
        ));
    }
    let raw = tile.data.as_u8().ok_or_else(|| {
        WsiError::UnsupportedFormat(".svcache builder expected uint8 tile data".into())
    })?;
    let color_space = ColorSpaceMeta::try_from(&tile.color_space)?;
    let encoded = zstd::bulk::compress(raw, 1).map_err(|err| WsiError::Codec {
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
        sha256: hex_encode(&Sha256::digest(&encoded)),
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
    let metadata_json = serde_json::to_vec(metadata).map_err(|err| WsiError::InvalidSlide {
        path: out_path.into(),
        message: format!("serialize svcache metadata: {err}"),
    })?;
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
