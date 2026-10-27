//! Native CZI reader benchmark, separate from the OpenSlide compatibility shim.
//! Run in release mode with WSI_RS_CZI_JXR_PATH and WSI_RS_CZI_PERF_OUTPUT set.
use std::{path::Path, time::Instant};

use serde_json::json;
use sha2::{Digest, Sha256};
use wsi_rs::{Slide, SlideOpenOptions, TileRequest};

#[allow(dead_code)]
#[path = "support/bench.rs"]
mod bench;

fn requests(path: &Path) -> (Vec<TileRequest>, Vec<TileRequest>, Vec<TileRequest>) {
    let slide = Slide::open(path).unwrap();
    let levels = &slide.dataset().scenes[0].series[0].levels;
    let acquired = |level: usize| {
        let dimensions = levels[level].dimensions;
        (0..dimensions.1.div_ceil(256))
            .flat_map(move |row| (0..dimensions.0.div_ceil(256)).map(move |col| (col, row)))
            .map(move |(col, row)| {
                TileRequest::new(0usize, 0usize, level as u32, col as i64, row as i64)
            })
            .filter(|request| slide.tile_is_present(request).unwrap() == Some(true))
            .collect::<Vec<_>>()
    };
    let grid = |level: usize| {
        let present = acquired(level);
        let first = present.first().expect("native CZI acquired tile");
        present
            .iter()
            .filter(|request| {
                (first.col..first.col + 4).contains(&request.col)
                    && (first.row..first.row + 4).contains(&request.row)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    let present = acquired(0);
    let step = present.len().div_ceil(16).max(1);
    let random = present.into_iter().step_by(step).take(16).collect();
    (grid(0), random, grid(2.min(levels.len() - 1)))
}

#[test]
#[ignore = "release benchmark requires WSI_RS_CZI_JXR_PATH and WSI_RS_CZI_PERF_OUTPUT"]
fn czi_reader_workloads() {
    let path = std::env::var_os("WSI_RS_CZI_JXR_PATH").expect("CZI corpus path");
    let path = Path::new(&path);
    let (pan, random, batch) = requests(path);
    let mut results = Vec::new();
    for profile in bench::CACHE_PROFILES {
        let cache = bench::cache_profile(profile);
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
                    let nonzero = tiles
                        .iter()
                        .any(|tile| tile.data().as_u8().unwrap().iter().any(|&b| b != 0));
                    assert!(nonzero, "benchmark must sample tissue");
                    let checksum = bench::tile_checksum(&tiles);
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
