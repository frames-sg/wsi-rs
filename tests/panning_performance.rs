//! Native panning matrix supplementing perf-runner's whole-slide diagonal trace.
//! Run with WSI_RS_PAN_PATH and WSI_RS_PAN_OUTPUT in a release build.
use std::{collections::BTreeMap, path::Path, time::Instant};

use serde_json::json;
use sha2::{Digest, Sha256};
use wsi_rs::{
    CacheConfig, CpuTile, DecodeAcceleration, DecodeExecutionOptions, RegionRequest, Slide,
    SlideOpenOptions, TileCache, TileLayout,
};

#[test]
#[ignore = "requires a real slide and WSI_RS_PAN_OUTPUT; run in release mode"]
fn panning_matrix() {
    let path = std::env::var_os("WSI_RS_PAN_PATH").expect("slide path");
    let path = Path::new(&path);
    let backend = std::env::var("WSI_RS_PAN_BACKEND").unwrap_or_else(|_| "auto".into());
    let acceleration = match backend.as_str() {
        "cpu" => DecodeAcceleration::CpuOnly,
        "auto" => DecodeAcceleration::Auto,
        _ => panic!("WSI_RS_PAN_BACKEND must be cpu or auto"),
    };
    let execution = DecodeExecutionOptions::default().with_acceleration(acceleration);
    let metadata = Slide::open_with_options(
        path,
        SlideOpenOptions::default().with_decode_execution_options(
            DecodeExecutionOptions::default().with_acceleration(DecodeAcceleration::CpuOnly),
        ),
    )
    .unwrap();
    let size: u32 = std::env::var("WSI_RS_PAN_SIZE")
        .unwrap_or_else(|_| "512".into())
        .parse()
        .unwrap();
    assert!((1..=2048).contains(&size));
    let alignment = std::env::var("WSI_RS_PAN_ALIGNMENT").unwrap_or_else(|_| "original".into());
    assert!(matches!(
        alignment.as_str(),
        "original" | "native" | "unaligned"
    ));
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
            let tile_size = match levels[level].tile_layout {
                TileLayout::Regular {
                    tile_width,
                    tile_height,
                    ..
                } => (tile_width, tile_height),
                TileLayout::WholeLevel {
                    virtual_tile_width,
                    virtual_tile_height,
                    ..
                } => (virtual_tile_width, virtual_tile_height),
                _ => (256, 256),
            };
            let anchor = if alignment == "original" {
                anchors[level]
            } else {
                let offset = i64::from(alignment == "unaligned");
                (
                    anchors[level].0 / i64::from(tile_size.0) * i64::from(tile_size.0) + offset,
                    anchors[level].1 / i64::from(tile_size.1) * i64::from(tile_size.1) + offset,
                )
            };
            let step = if alignment == "native" {
                i64::from(tile_size.0)
            } else {
                128
            };
            for name in ["pan", "background", "boundary", "scattered"] {
                let workload = format!("{name}_l{level}");
                if filter.as_ref().is_some_and(|f| !workload.contains(f)) {
                    continue;
                }
                let reqs: Vec<_> = (0..8)
                    .map(|i| {
                        let (x, y) = match name {
                            "pan" => (anchor.0 + i * step, anchor.1),
                            "background" => (i * 128, 0),
                            "boundary" => (width as i64 - 256 + i * 32, height as i64 - 256),
                            _ => (
                                ((i * 7919 + 1031) as u64 % width) as i64,
                                ((i * 3571 + 2027) as u64 % height) as i64,
                            ),
                        };
                        RegionRequest::new(0usize, 0usize, level as u32, (x, y), (size, size))
                    })
                    .collect();
                for repeat in 0..repeats {
                    let start = Instant::now();
                    let slide = Slide::open_with_options(
                        path,
                        SlideOpenOptions::default()
                            .with_cache_config(cache)
                            .with_decode_execution_options(execution),
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
                        let execution_before = execution_counters();
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
                        let execution_after = execution_counters();
                        let mut digest = Sha256::new();
                        let mut colored_pixels = 0usize;
                        for tile in &tiles {
                            assert_eq!((tile.width(), tile.height()), (size, size));
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
                            "elapsed_us": elapsed_us, "concurrency": concurrency, "backend": backend,
                            "size": size, "alignment": alignment,
                            "execution_before": execution_before, "execution_after": execution_after,
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

fn execution_counters() -> serde_json::Value {
    #[cfg(feature = "route-telemetry")]
    {
        serde_json::from_str(&wsi_rs::decode_route_telemetry_json()).unwrap()
    }
    #[cfg(not(feature = "route-telemetry"))]
    {
        serde_json::Value::Null
    }
}

/// Repeated warm traces resolve short-sequence jitter without timing verification.
#[test]
#[ignore = "requires WSI_RS_PAN_WARM_INPUT capture and WSI_RS_PAN_WARM_OUTPUT; release only"]
fn warm_region_revisits() {
    let input = std::env::var_os("WSI_RS_PAN_WARM_INPUT").expect("reference capture");
    let capture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&input).unwrap()).unwrap();
    let path = capture["source"].as_str().unwrap();
    let mut results = Vec::new();
    for row in capture["results"].as_array().unwrap().iter().filter(|row| {
        row["cache"] == "default" && row["phase"] == "warm_revisit" && row["repeat"] == 0
    }) {
        let size = u32::try_from(row["size"].as_u64().unwrap()).unwrap();
        assert!((1..=2048).contains(&size));
        let concurrency = usize::try_from(row["concurrency"].as_u64().unwrap()).unwrap();
        assert!((1..=4).contains(&concurrency));
        let workload = row["workload"].as_str().unwrap();
        let level: u32 = workload.rsplit_once("_l").unwrap().1.parse().unwrap();
        let requests = row["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|coord| {
                RegionRequest::new(
                    0usize,
                    0usize,
                    level,
                    (coord[0].as_i64().unwrap(), coord[1].as_i64().unwrap()),
                    (size, size),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(requests.len(), 8);
        let slide = Slide::open_with_options(
            path,
            SlideOpenOptions::default().with_decode_execution_options(
                DecodeExecutionOptions::default().with_acceleration(DecodeAcceleration::CpuOnly),
            ),
        )
        .unwrap();
        let expected = requests
            .iter()
            .map(|req| slide.read_region(req).unwrap())
            .collect::<Vec<_>>();
        let mut digest = Sha256::new();
        for tile in &expected {
            digest.update(tile.width().to_le_bytes());
            digest.update(tile.height().to_le_bytes());
            digest.update(tile.data().as_u8().unwrap());
        }
        assert_eq!(
            format!("{:x}", digest.finalize()),
            row["checksum"].as_str().unwrap()
        );
        let cycles = (64_u64 * 2048 * 2048 / u64::from(size).pow(2)).clamp(64, 4096);
        let times_us =
            measure_warm_region_sequences(&slide, &requests, &expected, concurrency, cycles);
        results.push(
            json!({"workload":workload,"size":size,"alignment":row["alignment"],
            "concurrency":concurrency,"cycles":cycles,"reader_calls":cycles*8,
            "mean_sequence_us":times_us.iter().sum::<f64>() / cycles as f64,
            "sequence_times_us":times_us,"requests":row["requests"],"checksum":row["checksum"]}),
        );
    }
    assert!(!results.is_empty());
    std::fs::write(std::env::var_os("WSI_RS_PAN_WARM_OUTPUT").expect("warm output"),
        serde_json::to_vec_pretty(&json!({"source":path,"input_capture":input,
            "timing":"Repeated eight-read sequences with retained caller threads; startup, exact validation and output disposal between sequences are excluded",
            "threads":rayon::current_num_threads(),"results":results})).unwrap()).unwrap();
}

fn measure_warm_region_sequences(
    slide: &Slide,
    requests: &[RegionRequest],
    expected: &[CpuTile],
    concurrency: usize,
    cycles: u64,
) -> Vec<f64> {
    std::thread::scope(|scope| {
        let (ready, ready_rx) = std::sync::mpsc::channel();
        let workers = (0..if concurrency == 1 { 0 } else { concurrency })
            .map(|_| {
                let (jobs, jobs_rx) = std::sync::mpsc::sync_channel::<usize>(1);
                let (results, results_rx) = std::sync::mpsc::sync_channel(1);
                let ready = ready.clone();
                scope.spawn(move || {
                    ready.send(()).unwrap();
                    while let Ok(index) = jobs_rx.recv() {
                        if results.send(slide.read_region(&requests[index])).is_err() {
                            break;
                        }
                    }
                });
                (jobs, results_rx)
            })
            .collect::<Vec<_>>();
        for _ in &workers {
            ready_rx.recv().unwrap();
        }
        let mut times_us = Vec::with_capacity(cycles as usize);
        for _ in 0..cycles {
            let start = Instant::now();
            let actual = if concurrency == 1 {
                requests
                    .iter()
                    .map(|req| slide.read_region(req).unwrap())
                    .collect::<Vec<_>>()
            } else {
                let mut actual = Vec::with_capacity(requests.len());
                for (wave_index, wave) in requests.chunks(concurrency).enumerate() {
                    for (index, (jobs, _)) in workers.iter().take(wave.len()).enumerate() {
                        jobs.send(wave_index * concurrency + index).unwrap();
                    }
                    for (_, results) in workers.iter().take(wave.len()) {
                        actual.push(results.recv().unwrap().unwrap());
                    }
                }
                actual
            };
            times_us.push(start.elapsed().as_secs_f64() * 1e6);
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(
                    (actual.width(), actual.height()),
                    (expected.width(), expected.height())
                );
                assert!(
                    actual.data().as_u8() == expected.data().as_u8(),
                    "warm pixels changed"
                );
            }
        }
        // Closing job channels also lets workers exit when an assertion unwinds.
        drop(workers);
        times_us
    })
}
