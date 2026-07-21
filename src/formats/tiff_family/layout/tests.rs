use super::*;
use crate::core::types::{AxesShape, DatasetId, Level, SampleType, Scene, Series, TileLayout};
use std::collections::HashSet;

// ── TileSourceKey ──────────────────────────────────────────────────────────

#[test]
fn tile_source_key_equality() {
    let a = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
    };
    let b = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
    };
    assert_eq!(a, b);
}

#[test]
fn tile_source_key_inequality_on_each_field() {
    let base = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
    };
    let cases: &[TileSourceKey] = &[
        TileSourceKey {
            scene: 1,
            ..base.clone()
        },
        TileSourceKey {
            series: 1,
            ..base.clone()
        },
        TileSourceKey {
            level: 1u32,
            ..base.clone()
        },
        TileSourceKey {
            z: 1,
            ..base.clone()
        },
        TileSourceKey {
            c: 1,
            ..base.clone()
        },
        TileSourceKey {
            t: 1,
            ..base.clone()
        },
    ];
    for key in cases {
        assert_ne!(base, *key, "expected {:?} != {:?}", base, key);
    }
}

#[test]
fn tile_source_key_hash_consistency() {
    let mut set = HashSet::new();
    let key = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 2u32,
        z: 0,
        c: 0,
        t: 0,
    };
    set.insert(key.clone());
    set.insert(key.clone());
    assert_eq!(set.len(), 1);
}

#[test]
fn tile_source_key_distinct_keys_in_hashmap() {
    let mut map: HashMap<TileSourceKey, u32> = HashMap::new();
    for level in 0..4u32 {
        let key = TileSourceKey {
            scene: 0usize,
            series: 0usize,
            level,
            z: 0,
            c: 0,
            t: 0,
        };
        map.insert(key, level * 10);
    }
    assert_eq!(map.len(), 4);
    let k = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 2u32,
        z: 0,
        c: 0,
        t: 0,
    };
    assert_eq!(map[&k], 20);
}

// ── TileSource construction ────────────────────────────────────────────────

#[test]
fn tile_source_tiled_ifd_construction() {
    let src = TileSource::TiledIfd {
        ifd_id: IfdId(512),
        jpeg_tables: Some(vec![0xFF, 0xD8]),
        compression: Compression::Jpeg,
    };
    match src {
        TileSource::TiledIfd {
            ifd_id,
            jpeg_tables,
            compression,
        } => {
            assert_eq!(ifd_id, IfdId(512));
            assert!(jpeg_tables.is_some());
            assert_eq!(compression, Compression::Jpeg);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn tile_source_ndpi_restart_construction() {
    let src = TileSource::NdpiJpeg {
        ifd_id: IfdId(1024),
        jpeg_header: vec![0xFF, 0xD8, 0xFF, 0xC0],
        mcu_starts_tag: 65426,
        tiles_across: 8,
        tiles_down: 6,
        restart_interval: 16,
        strip_offset: 4096,
        strip_byte_count: 1_000_000,
    };
    match src {
        TileSource::NdpiJpeg {
            ifd_id,
            tiles_across,
            tiles_down,
            restart_interval,
            strip_offset,
            strip_byte_count,
            mcu_starts_tag,
            ..
        } => {
            assert_eq!(ifd_id, IfdId(1024));
            assert_eq!(tiles_across, 8);
            assert_eq!(tiles_down, 6);
            assert_eq!(restart_interval, 16);
            assert_eq!(strip_offset, 4096);
            assert_eq!(strip_byte_count, 1_000_000);
            assert_eq!(mcu_starts_tag, 65426);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn tile_source_ndpi_full_decode_construction() {
    let src = TileSource::NdpiFullDecode {
        ifd_id: IfdId(2048),
        jpeg_header: vec![0xFF, 0xD8],
        strip_offset: 8192,
        strip_byte_count: 500_000,
    };
    match src {
        TileSource::NdpiFullDecode {
            ifd_id,
            strip_offset,
            strip_byte_count,
            ..
        } => {
            assert_eq!(ifd_id, IfdId(2048));
            assert_eq!(strip_offset, 8192);
            assert_eq!(strip_byte_count, 500_000);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn tile_source_stripped_construction() {
    let src = TileSource::Stripped {
        ifd_id: IfdId(4096),
        jpeg_tables: None,
        compression: Compression::None,
        strip_offsets: vec![0],
        strip_byte_counts: vec![0],
    };
    match src {
        TileSource::Stripped {
            ifd_id,
            compression,
            ..
        } => {
            assert_eq!(ifd_id, IfdId(4096));
            assert_eq!(compression, Compression::None);
        }
        _ => panic!("wrong variant"),
    }
}

// ── DatasetLayout construction ─────────────────────────────────────────────

fn make_minimal_dataset() -> Dataset {
    Dataset {
        id: DatasetId::new(1),
        scenes: vec![Scene {
            id: "scene-0".into(),
            name: None,
            series: vec![Series {
                id: "series-0".into(),
                axes: AxesShape::default(),
                levels: vec![Level {
                    dimensions: (1024, 768),
                    downsample: 1.0,
                    tile_layout: TileLayout::Regular {
                        tile_width: 256,
                        tile_height: 256,
                        tiles_across: 4,
                        tiles_down: 3,
                    },
                }],
                sample_type: SampleType::Uint8,
                channels: vec![],
            }],
        }],
        associated_images: HashMap::new(),
        properties: Default::default(),
        icc_profiles: HashMap::new(),
        source_icc_profiles: Vec::new(),
    }
}

#[test]
fn dataset_layout_construction_with_tile_sources() {
    let key = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 0u32,
        z: 0,
        c: 0,
        t: 0,
    };
    let source = TileSource::TiledIfd {
        ifd_id: IfdId(8),
        jpeg_tables: None,
        compression: Compression::Jpeg,
    };

    let mut tile_sources = HashMap::new();
    tile_sources.insert(key.clone(), source);

    let layout = DatasetLayout {
        dataset: make_minimal_dataset(),
        tile_sources,
        associated_sources: HashMap::new(),
    };

    assert_eq!(layout.dataset.id, DatasetId::new(1));
    assert!(layout.tile_sources.contains_key(&key));
    assert!(layout.associated_sources.is_empty());
}

#[test]
fn dataset_layout_construction_with_associated_sources() {
    let macro_src = TileSource::Stripped {
        ifd_id: IfdId(256),
        jpeg_tables: None,
        compression: Compression::Jpeg,
        strip_offsets: vec![0],
        strip_byte_counts: vec![0],
    };

    let mut associated_sources = HashMap::new();
    associated_sources.insert("macro".to_string(), macro_src);

    let layout = DatasetLayout {
        dataset: make_minimal_dataset(),
        tile_sources: HashMap::new(),
        associated_sources,
    };

    assert!(layout.associated_sources.contains_key("macro"));
    assert!(layout.tile_sources.is_empty());
}

#[test]
fn dataset_layout_multiple_levels() {
    let mut tile_sources = HashMap::new();
    for level in 0..4u32 {
        let key = TileSourceKey {
            scene: 0usize,
            series: 0usize,
            level,
            z: 0,
            c: 0,
            t: 0,
        };
        let src = TileSource::TiledIfd {
            ifd_id: IfdId(level as u64 * 512),
            jpeg_tables: None,
            compression: Compression::Jpeg,
        };
        tile_sources.insert(key, src);
    }

    let layout = DatasetLayout {
        dataset: make_minimal_dataset(),
        tile_sources,
        associated_sources: HashMap::new(),
    };

    assert_eq!(layout.tile_sources.len(), 4);
    let k2 = TileSourceKey {
        scene: 0usize,
        series: 0usize,
        level: 2u32,
        z: 0,
        c: 0,
        t: 0,
    };
    assert!(layout.tile_sources.contains_key(&k2));
}
