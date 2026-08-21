use super::*;

pub(super) fn single_level_svcache_metadata(
    source_path: &std::path::Path,
    complete: bool,
    tiles_across: u64,
    tiles_down: u64,
    tiles: Vec<Option<TileMeta>>,
) -> SvcacheMetadata {
    SvcacheMetadata {
        schema_version: SCHEMA_VERSION,
        complete,
        source: fingerprint_source(source_path).unwrap(),
        properties: Vec::new(),
        scenes: vec![SceneMeta {
            id: "scene-0".into(),
            name: None,
            series: vec![SeriesMeta {
                id: "series-0".into(),
                axes: AxesMeta { z: 1, c: 1, t: 1 },
                sample_type: SampleTypeMeta::Uint8,
                channels: Vec::new(),
                levels: vec![LevelMeta {
                    dimensions: (tiles_across, tiles_down),
                    downsample: 1.0,
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across,
                    tiles_down,
                    tiles,
                    sparse_tiles: Vec::new(),
                }],
            }],
        }],
        associated: Vec::new(),
    }
}

pub(super) fn raw_jp2k_source() -> tempfile::NamedTempFile {
    let mut source = tempfile::Builder::new().suffix(".j2c").tempfile().unwrap();
    source
        .write_all(include_bytes!(
            "../../../../tests/fixtures/jp2k/rgb_nomct.j2k"
        ))
        .unwrap();
    source.flush().unwrap();
    source
}
