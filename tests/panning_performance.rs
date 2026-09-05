//! Native panning matrix supplementing perf-runner's whole-slide diagonal trace.
//! Run with WSI_RS_PAN_PATH and WSI_RS_PAN_OUTPUT in a release build.
use std::{collections::BTreeMap, path::Path, time::Instant};

use serde_json::json;
use sha2::{Digest, Sha256};
use wsi_rs::{CacheConfig, RegionRequest, Slide, SlideOpenOptions, TileCache};

#[test]
#[ignore = "requires a real slide and WSI_RS_PAN_OUTPUT; run in release mode"]
fn panning_matrix() {
    let path = std::env::var_os("WSI_RS_PAN_PATH").expect("slide path");
    let path = Path::new(&path);
    let metadata = Slide::open(path).unwrap();
    let levels = &metadata.dataset().scenes[0].series[0].levels;
    // Select the most chromatic of nine deterministic candidates outside timing.
    let anchors: Vec<_> = levels
        .iter()
        .enumerate()
        .map(|(level, info)| {
            if level != 0 && level != 2.min(levels.len() - 1) {
                return (0, 0);
            }
            (1..=3)
                .flat_map(|y| (1..=3).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let origin = (
                        (info.dimensions.0 * x / 4) as i64,
                        (info.dimensions.1 * y / 4) as i64,
                    );
                    let tile = metadata
                        .read_region(&RegionRequest::new(
                            0usize,
                            0usize,
                            level as u32,
                            origin,
                            (128, 128),
                        ))
                        .unwrap();
                    let score = tile
                        .data()
                        .as_u8()
                        .unwrap()
                        .chunks_exact(3)
                        .filter(|p| p.iter().max().unwrap() - p.iter().min().unwrap() > 20)
                        .count();
                    (score, origin)
                })
                .max()
                .unwrap()
                .1
        })
        .collect();
    let repeats: usize = std::env::var("WSI_RS_PAN_REPEATS")
        .unwrap_or_else(|_| "3".into())
        .parse()
        .unwrap();
    let filter = std::env::var("WSI_RS_PAN_ONLY").ok();
    let concurrency: usize = std::env::var("WSI_RS_PAN_CONCURRENT")
        .unwrap_or_else(|_| "1".into())
        .parse()
        .unwrap();
    assert!((1..=4).contains(&concurrency));
    let mut results = Vec::new();
    let mut expected = BTreeMap::new();
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
        for level in [0, 2.min(levels.len() - 1)] {
            let (width, height) = levels[level].dimensions;
            for name in ["pan", "background", "boundary", "scattered"] {
                let workload = format!("{name}_l{level}");
                if filter.as_ref().is_some_and(|f| !workload.contains(f)) {
                    continue;
                }
                let reqs: Vec<_> = (0..8)
                    .map(|i| {
                        let (x, y) = match name {
                            "pan" => (anchors[level].0 + i * 128, anchors[level].1),
                            "background" => (i * 128, 0),
                            "boundary" => (width as i64 - 256 + i * 32, height as i64 - 256),
                            _ => (
                                ((i * 7919 + 1031) as u64 % width) as i64,
                                ((i * 3571 + 2027) as u64 % height) as i64,
                            ),
                        };
                        RegionRequest::new(0usize, 0usize, level as u32, (x, y), (512, 512))
                    })
                    .collect();
                for repeat in 0..repeats {
                    let start = Instant::now();
                    let slide = Slide::open_with_options(
                        path,
                        SlideOpenOptions::default().with_cache_config(cache),
                    )
                    .unwrap();
                    let open_us = start.elapsed().as_secs_f64() * 1e6;
                    let observed_cache = std::sync::Arc::new(TileCache::new(
                        cache.shared_tile_bytes.unwrap_or(64 * 1024 * 1024),
                    ));
                    slide.replace_shared_tile_cache(observed_cache.clone());
                    for phase in ["cold_reader", "warm_revisit"] {
                        let cache_before = observed_cache.stats();
                        let mut latencies = Vec::new();
                        let mut tiles = Vec::new();
                        let sequence_start = Instant::now();
                        if concurrency == 1 {
                            for req in &reqs {
                                let start = Instant::now();
                                tiles.push(slide.read_region(req).unwrap());
                                latencies.push(start.elapsed().as_secs_f64() * 1e6);
                            }
                        } else {
                            for wave in reqs.chunks(concurrency) {
                                let barrier = std::sync::Barrier::new(wave.len());
                                let results = std::thread::scope(|scope| {
                                    let handles: Vec<_> = wave
                                        .iter()
                                        .map(|req| {
                                            let slide = &slide;
                                            let barrier = &barrier;
                                            scope.spawn(move || {
                                                barrier.wait();
                                                let start = Instant::now();
                                                let tile = slide.read_region(req).unwrap();
                                                (tile, start.elapsed().as_secs_f64() * 1e6)
                                            })
                                        })
                                        .collect();
                                    handles
                                        .into_iter()
                                        .map(|h| h.join().unwrap())
                                        .collect::<Vec<_>>()
                                });
                                for (tile, latency) in results {
                                    tiles.push(tile);
                                    latencies.push(latency);
                                }
                            }
                        }
                        let elapsed_us = sequence_start.elapsed().as_secs_f64() * 1e6;
                        let mut digest = Sha256::new();
                        let mut colored_pixels = 0usize;
                        for tile in &tiles {
                            assert_eq!((tile.width(), tile.height()), (512, 512));
                            digest.update(tile.width().to_le_bytes());
                            digest.update(tile.height().to_le_bytes());
                            let bytes = tile.data().as_u8().unwrap();
                            digest.update(bytes);
                            colored_pixels += bytes
                                .chunks_exact(3)
                                .filter(|p| p.iter().max().unwrap() - p.iter().min().unwrap() > 20)
                                .count();
                        }
                        let checksum = format!("{:x}", digest.finalize());
                        assert_eq!(
                            expected
                                .entry(workload.clone())
                                .or_insert_with(|| checksum.clone()),
                            &checksum
                        );
                        results.push(
                            json!({"cache": profile, "workload": workload, "repeat": repeat,
                            "phase": phase, "open_us": open_us, "latencies_us": latencies,
                            "elapsed_us": elapsed_us, "concurrency": concurrency,
                            "source_puts": observed_cache.stats().puts - cache_before.puts,
                            "source_entries": observed_cache.stats().entries,
                            "source_evictions": observed_cache.stats().evictions - cache_before.evictions,
                            "checksum": checksum, "colored_pixels": colored_pixels,
                            "requests": reqs.iter().map(|r| r.origin_px).collect::<Vec<_>>()}),
                        );
                    }
                }
            }
        }
    }
    let output = std::env::var_os("WSI_RS_PAN_OUTPUT").expect("output path");
    std::fs::write(output, serde_json::to_vec_pretty(&json!({"source": path,
        "levels": levels.iter().map(|l| json!({"dimensions": l.dimensions, "downsample": l.downsample, "layout": format!("{:?}", l.tile_layout)})).collect::<Vec<_>>(),
        "os_cache": "uncontrolled; cold_reader means a newly opened slide, not cold filesystem",
        "results": results})).unwrap()).unwrap();
}
