//! Native CZI reader benchmark, separate from the OpenSlide compatibility shim.
//! Run in release mode with WSI_RS_CZI_JXR_PATH and WSI_RS_CZI_PERF_OUTPUT set.
use std::{collections::BTreeSet, path::Path, time::Instant};

use serde_json::json;
use sha2::{Digest, Sha256};
use wsi_rs::{CacheConfig, Slide, SlideOpenOptions, TileRequest};

fn requests(path: &Path) -> (Vec<TileRequest>, Vec<TileRequest>, Vec<TileRequest>) {
    let source = czi_rs::CziFile::open(path).unwrap();
    let slide = Slide::open(path).unwrap();
    let levels = &slide.dataset().scenes[0].series[0].levels;
    let origin = (
        source.subblocks().iter().map(|b| b.rect.x).min().unwrap(),
        source.subblocks().iter().map(|b| b.rect.y).min().unwrap(),
    );
    let center = |level: usize, block: &czi_rs::DirectorySubBlockInfo| {
        let ratio = levels[level].downsample.round() as i64;
        (
            ((i64::from(block.rect.x) - i64::from(origin.0)) / ratio
                + i64::from(block.stored_size.w / 2))
                / 256,
            ((i64::from(block.rect.y) - i64::from(origin.1)) / ratio
                + i64::from(block.stored_size.h / 2))
                / 256,
        )
    };
    let blocks_at = |level: usize| {
        let ratio = levels[level].downsample.round() as i64;
        source
            .subblocks()
            .iter()
            .filter(move |b| i64::from(b.rect.w) == i64::from(b.stored_size.w) * ratio)
            .collect::<Vec<_>>()
    };
    let grid = |level: usize| {
        let blocks = blocks_at(level);
        let (x, y) = center(level, blocks[0]);
        let dims = levels[level].dimensions;
        (0..4)
            .flat_map(|dy| (0..4).map(move |dx| (x + dx, y + dy)))
            .filter(|&(col, row)| {
                col >= 0 && row >= 0 && (col as u64) * 256 < dims.0 && (row as u64) * 256 < dims.1
            })
            .map(|(col, row)| TileRequest::new(0usize, 0usize, level as u32, col, row))
            .collect::<Vec<_>>()
    };
    let blocks = blocks_at(0);
    let mut seen = BTreeSet::new();
    let random = (0..blocks.len())
        .map(|i| (i * 7919) % blocks.len())
        .filter_map(|i| {
            let (x, y) = center(0, blocks[i]);
            seen.insert((x, y))
                .then(|| TileRequest::new(0usize, 0usize, 0, x, y))
        })
        .take(16)
        .collect();
    (grid(0), random, grid(2.min(levels.len() - 1)))
}

#[test]
#[ignore = "release benchmark requires WSI_RS_CZI_JXR_PATH and WSI_RS_CZI_PERF_OUTPUT"]
fn czi_reader_workloads() {
    let path = std::env::var_os("WSI_RS_CZI_JXR_PATH").expect("CZI corpus path");
    let path = Path::new(&path);
    let (pan, random, batch) = requests(path);
    let mut results = Vec::new();
    for (profile, cache) in [
        ("default", CacheConfig::default()),
        (
            "disabled",
            CacheConfig::default()
                .with_shared_tile_bytes(0)
                .with_display_tile_bytes(0),
        ),
        (
            "small",
            CacheConfig::default()
                .with_shared_tile_bytes(1024 * 1024)
                .with_display_tile_bytes(0),
        ),
    ] {
        for (name, reqs, batched) in [
            ("pan_l0", &pan, false),
            ("random_l0", &random, false),
            ("batch_l2", &batch, true),
        ] {
            let mut expected = None;
            for repeat in 0..3 {
                let start = Instant::now();
                let slide = Slide::open_with_options(
                    path,
                    SlideOpenOptions::default().with_cache_config(cache),
                )
                .unwrap();
                let open_us = start.elapsed().as_secs_f64() * 1e6;
                for phase in ["cold", "warm"] {
                    let mut latencies = Vec::new();
                    let tiles = if batched {
                        let start = Instant::now();
                        let tiles = slide.read_tiles(reqs).unwrap();
                        latencies.push(start.elapsed().as_secs_f64() * 1e6);
                        tiles
                    } else {
                        reqs.iter()
                            .map(|req| {
                                let start = Instant::now();
                                let tile = slide.read_tile(req).unwrap();
                                latencies.push(start.elapsed().as_secs_f64() * 1e6);
                                tile
                            })
                            .collect()
                    };
                    assert_eq!(tiles.len(), reqs.len());
                    let mut digest = Sha256::new();
                    let mut nonzero = false;
                    for tile in &tiles {
                        let bytes = tile.data().as_u8().unwrap();
                        nonzero |= bytes.iter().any(|&b| b != 0);
                        digest.update(tile.width().to_le_bytes());
                        digest.update(tile.height().to_le_bytes());
                        digest.update(bytes);
                    }
                    assert!(nonzero, "benchmark must sample tissue");
                    let checksum = format!("{:x}", digest.finalize());
                    assert_eq!(expected.get_or_insert_with(|| checksum.clone()), &checksum);
                    results.push(json!({"cache": profile, "workload": name, "phase": phase, "repeat": repeat,
                        "open_us": open_us, "requests": reqs.len(), "latencies_us": latencies, "checksum": checksum}));
                }
            }
        }
    }
    let output = std::env::var_os("WSI_RS_CZI_PERF_OUTPUT").expect("benchmark output path");
    let report = json!({"source": path, "source_sha256": format!("{:x}", Sha256::digest(std::fs::read(path).unwrap())),
        "os_cache": "uncontrolled; cold means a newly opened slide", "results": results});
    std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
