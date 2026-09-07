//! Explicit native CPU, adaptive, resident-Metal and readback benchmark.
//! Validation and warmup are outside timed calls. Run this ignored test in release mode.
use std::{path::Path, time::Instant};

use serde_json::json;
use wsi_rs::{
    CacheConfig, CpuTile, DecodeAcceleration, DecodeExecutionOptions, Slide, SlideOpenOptions,
    TileLayout, TileRequest,
};

#[allow(dead_code)]
#[path = "support/compare.rs"]
mod compare;

#[test]
#[ignore = "requires WSI_RS_DECODE_PATH and WSI_RS_DECODE_OUTPUT; run in release mode"]
fn native_decode_matrix() {
    let path = std::env::var_os("WSI_RS_DECODE_PATH").expect("slide path");
    let path = Path::new(&path);
    let mode = std::env::var("WSI_RS_DECODE_MODE").unwrap_or_else(|_| "cpu".into());
    assert!(matches!(
        mode.as_str(),
        "cpu" | "auto" | "metal" | "metal-download"
    ));
    #[cfg(not(feature = "metal"))]
    assert!(
        !mode.starts_with("metal"),
        "strict Metal requires the metal feature"
    );
    let repeats: usize = std::env::var("WSI_RS_DECODE_REPEATS")
        .unwrap_or_else(|_| "3".into())
        .parse()
        .unwrap();
    let only_batch = std::env::var("WSI_RS_DECODE_BATCH")
        .ok()
        .map(|n| n.parse::<usize>().unwrap());
    let reference = Slide::open_with_options(path, options(CacheConfig::default(), true)).unwrap();
    let levels = &reference.dataset().scenes[0].series[0].levels;
    #[cfg(feature = "metal")]
    let sessions = mode.starts_with("metal").then(|| {
        wsi_rs::output::metal::MetalBackendSessions::system_default().expect("Metal is required")
    });
    let concurrency: usize = std::env::var("WSI_RS_DECODE_CONCURRENT")
        .unwrap_or_else(|_| "1".into())
        .parse()
        .unwrap();
    assert!((1..=2).contains(&concurrency));
    let mut results = Vec::new();
    for level in [0, 2.min(levels.len() - 1)] {
        let TileLayout::Regular {
            tiles_across,
            tiles_down,
            ..
        } = levels[level].tile_layout
        else {
            panic!("native decode benchmark requires regular source tiles");
        };
        for count in [1, 4, 8, 16, 64] {
            if only_batch.is_some_and(|selected| selected != count) {
                continue;
            }
            let reqs: Vec<_> = (0..count)
                .map(|i| {
                    TileRequest::new(
                        0usize,
                        0usize,
                        level as u32,
                        ((tiles_across / 2 + (i % 8) as u64) % tiles_across) as i64,
                        ((tiles_down / 2 + (i / 8) as u64) % tiles_down) as i64,
                    )
                })
                .collect();
            let expected = reference.read_tiles(&reqs).unwrap();
            for (cache_name, cache) in [
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
                for repeat in 0..repeats {
                    let slide =
                        Slide::open_with_options(path, options(cache, mode == "cpu")).unwrap();
                    for iteration in 0..7 {
                        let execution_before = execution_counters();
                        let decode = || {
                            let start = Instant::now();
                            let pending = match mode.as_str() {
                                "cpu" | "auto" => {
                                    PendingValidation::Cpu(slide.read_tiles(&reqs).unwrap())
                                }
                                #[cfg(feature = "metal")]
                                "metal" | "metal-download" => {
                                    let resident = slide
                                        .read_tiles_metal(&reqs, sessions.as_ref().unwrap())
                                        .unwrap();
                                    if mode == "metal" {
                                        PendingValidation::Metal(resident)
                                    } else {
                                        PendingValidation::Cpu(
                                            resident
                                                .iter()
                                                .map(|tile| tile.download_cpu().unwrap())
                                                .collect(),
                                        )
                                    }
                                }
                                _ => unreachable!(),
                            };
                            (pending, start.elapsed().as_secs_f64() * 1e6)
                        };
                        let wall_start = Instant::now();
                        let barrier = std::sync::Barrier::new(concurrency);
                        let pending = if concurrency == 1 {
                            vec![decode()]
                        } else {
                            std::thread::scope(|scope| {
                                let workers = (0..concurrency)
                                    .map(|_| {
                                        let barrier = &barrier;
                                        let decode = &decode;
                                        scope.spawn(move || {
                                            barrier.wait();
                                            decode()
                                        })
                                    })
                                    .collect::<Vec<_>>();
                                workers
                                    .into_iter()
                                    .map(|worker| worker.join().unwrap())
                                    .collect()
                            })
                        };
                        let wall_elapsed_us = wall_start.elapsed().as_secs_f64() * 1e6;
                        let elapsed_us = pending
                            .iter()
                            .map(|(_, elapsed)| *elapsed)
                            .fold(0.0_f64, f64::max);
                        let execution_after = execution_counters();
                        let mut max_error = 0;
                        let mut bytes = 0;
                        for (pending, _) in pending {
                            let tiles = pending.into_cpu();
                            max_error = max_error.max(validate(&tiles, &expected, mode == "cpu"));
                            bytes += tiles
                                .iter()
                                .map(|tile| tile.data().byte_size())
                                .sum::<usize>();
                        }
                        if iteration == 0 || iteration == 6 {
                            results.push(json!({"mode": mode, "cache": cache_name, "level": level,
                                "batch": count, "repeat": repeat,
                                "phase": if iteration == 0 { "fresh_reader" } else { "warm_revisit" },
                                "elapsed_us": elapsed_us, "wall_elapsed_us": wall_elapsed_us,
                                "concurrency": concurrency, "max_abs_error": max_error,
                                "execution_before": execution_before, "execution_after": execution_after,
                                "bytes": bytes,
                                "requests": reqs.iter().map(|r| (r.col, r.row)).collect::<Vec<_>>()}));
                        }
                    }
                }
            }
        }
    }
    let output = std::env::var_os("WSI_RS_DECODE_OUTPUT").expect("benchmark output path");
    std::fs::write(output, serde_json::to_vec_pretty(&json!({"source": path,
        "mode": mode, "threads": rayon::current_num_threads(),
        "validation": "CPU exact; device uses existing JP2K lossy tolerance; checks outside timing",
        "os_cache": "uncontrolled; preparation uses a CPU-only reference reader", "results": results})).unwrap()).unwrap();
}

fn options(cache: CacheConfig, cpu_only: bool) -> SlideOpenOptions {
    SlideOpenOptions::default()
        .with_cache_config(cache)
        .with_decode_execution_options(DecodeExecutionOptions::default().with_acceleration(
            if cpu_only {
                DecodeAcceleration::CpuOnly
            } else {
                DecodeAcceleration::Auto
            },
        ))
}

fn validate(tiles: &[CpuTile], expected: &[CpuTile], exact: bool) -> u8 {
    assert_eq!(tiles.len(), expected.len());
    let mut max_error = 0;
    for (tile, expected) in tiles.iter().zip(expected) {
        assert_eq!(
            (tile.width(), tile.height()),
            (expected.width(), expected.height())
        );
        let report = compare::compare_rgba(
            tile.to_rgba().unwrap().as_raw(),
            expected.to_rgba().unwrap().as_raw(),
            if exact {
                compare::Tolerance::EXACT
            } else {
                compare::Tolerance::TOLERANT
            },
        );
        assert!(
            report.passed,
            "decoded batch failed CPU reference comparison: {report:?}"
        );
        max_error = max_error.max(report.max_abs);
    }
    max_error
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

enum PendingValidation {
    Cpu(Vec<wsi_rs::CpuTile>),
    #[cfg(feature = "metal")]
    Metal(Vec<wsi_rs::output::metal::MetalDeviceTile>),
}

impl PendingValidation {
    fn into_cpu(self) -> Vec<wsi_rs::CpuTile> {
        match self {
            Self::Cpu(tiles) => tiles,
            #[cfg(feature = "metal")]
            Self::Metal(tiles) => tiles
                .iter()
                .map(|tile| tile.download_cpu().unwrap())
                .collect(),
        }
    }
}

/// Longer controls for sub-millisecond native calls. Each capture identifies
/// the original matrix cells; validation and caller startup stay outside timing.
#[test]
#[ignore = "requires WSI_RS_DECODE_STEADY_INPUT and WSI_RS_DECODE_OUTPUT"]
fn native_decode_revisits() {
    let input = std::env::var_os("WSI_RS_DECODE_STEADY_INPUT").expect("input capture");
    let capture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap();
    let path = Path::new(capture["source"].as_str().unwrap());
    let mode = capture["mode"].as_str().unwrap();
    assert!(matches!(mode, "cpu" | "auto"));
    let reference = Slide::open_with_options(path, options(CacheConfig::default(), true)).unwrap();
    let mut results = Vec::new();
    for row in capture["results"].as_array().unwrap() {
        if row["phase"] != "warm_revisit" {
            continue;
        }
        let cache = match row["cache"].as_str().unwrap() {
            "default" => CacheConfig::default(),
            "disabled" => CacheConfig::default()
                .with_shared_tile_bytes(0)
                .with_display_tile_bytes(0),
            "small" => CacheConfig::default()
                .with_shared_tile_bytes(1024 * 1024)
                .with_display_tile_bytes(0),
            _ => unreachable!(),
        };
        let requests = row["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                TileRequest::new(
                    0usize,
                    0usize,
                    row["level"].as_u64().unwrap() as u32,
                    pair[0].as_i64().unwrap(),
                    pair[1].as_i64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let expected = reference.read_tiles(&requests).unwrap();
        let slide = Slide::open_with_options(path, options(cache, mode == "cpu")).unwrap();
        for _ in 0..7 {
            validate(
                &slide.read_tiles(&requests).unwrap(),
                &expected,
                mode == "cpu",
            );
        }
        let callers = row["concurrency"].as_u64().unwrap() as usize;
        let cycles = if row["cache"] == "default" {
            (4096 / requests.len()).clamp(64, 4096)
        } else {
            (256 / requests.len()).clamp(16, 256)
        };
        let times = std::thread::scope(|scope| {
            let (ready, ready_rx) = std::sync::mpsc::channel();
            let workers = (0..if callers == 1 { 0 } else { callers })
                .map(|_| {
                    let (jobs, jobs_rx) = std::sync::mpsc::sync_channel::<()>(1);
                    let (done, done_rx) = std::sync::mpsc::sync_channel(1);
                    let ready = ready.clone();
                    let slide = &slide;
                    let requests = &requests;
                    scope.spawn(move || {
                        ready.send(()).unwrap();
                        while jobs_rx.recv().is_ok() {
                            if done.send(slide.read_tiles(requests)).is_err() {
                                break;
                            }
                        }
                    });
                    (jobs, done_rx)
                })
                .collect::<Vec<_>>();
            for _ in &workers {
                ready_rx.recv().unwrap();
            }
            let mut times = Vec::with_capacity(cycles);
            for _ in 0..cycles {
                let start = Instant::now();
                let actual = if callers == 1 {
                    vec![slide.read_tiles(&requests).unwrap()]
                } else {
                    for (jobs, _) in &workers {
                        jobs.send(()).unwrap();
                    }
                    workers
                        .iter()
                        .map(|(_, done)| done.recv().unwrap().unwrap())
                        .collect()
                };
                times.push(start.elapsed().as_secs_f64() * 1e6);
                for tiles in actual {
                    validate(&tiles, &expected, mode == "cpu");
                }
            }
            drop(workers);
            times
        });
        results.push(json!({"cache":row["cache"],"level":row["level"],"batch":row["batch"],
            "concurrency":callers,"cycles":cycles,"requests":row["requests"],
            "mean_sequence_us":times.iter().sum::<f64>() / cycles as f64,"sequence_times_us":times}));
    }
    assert!(!results.is_empty());
    std::fs::write(
        std::env::var_os("WSI_RS_DECODE_OUTPUT").expect("output"),
        serde_json::to_vec_pretty(&json!({"source":path,"mode":mode,"results":results,
            "timing":"Retained caller threads; startup, validation and output disposal excluded"}))
        .unwrap(),
    )
    .unwrap();
}

/// Repeated fresh-reader controls for millisecond-scale matrix outliers. Opening
/// and validation are excluded; concurrent wall timing retains caller startup.
#[test]
#[ignore = "requires a selected WSI_RS_DECODE_STEADY_INPUT capture and WSI_RS_DECODE_OUTPUT"]
fn native_decode_fresh_readers() {
    let input = std::env::var_os("WSI_RS_DECODE_STEADY_INPUT").expect("input capture");
    let capture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap();
    let path = Path::new(capture["source"].as_str().unwrap());
    let mode = capture["mode"].as_str().unwrap();
    assert!(matches!(mode, "cpu" | "auto"));
    let reference = Slide::open_with_options(path, options(CacheConfig::default(), true)).unwrap();
    let mut results = Vec::new();
    for row in capture["results"].as_array().unwrap() {
        assert_eq!(row["phase"], "fresh_reader");
        let cache = match row["cache"].as_str().unwrap() {
            "default" => CacheConfig::default(),
            "disabled" => CacheConfig::default()
                .with_shared_tile_bytes(0)
                .with_display_tile_bytes(0),
            "small" => CacheConfig::default()
                .with_shared_tile_bytes(1024 * 1024)
                .with_display_tile_bytes(0),
            _ => unreachable!(),
        };
        let requests = row["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                TileRequest::new(
                    0usize,
                    0usize,
                    row["level"].as_u64().unwrap() as u32,
                    pair[0].as_i64().unwrap(),
                    pair[1].as_i64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let expected = reference.read_tiles(&requests).unwrap();
        // These controls target later fresh readers in the existing matrix,
        // which may reuse process-wide route decisions. Startup has separate
        // first-read telemetry and deterministic ownership regressions.
        let warmup = Slide::open_with_options(path, options(cache, mode == "cpu")).unwrap();
        for _ in 0..7 {
            validate(
                &warmup.read_tiles(&requests).unwrap(),
                &expected,
                mode == "cpu",
            );
        }
        drop(warmup);
        let callers = row["concurrency"].as_u64().unwrap() as usize;
        let mut times = Vec::new();
        for _ in 0..32 {
            let slide = Slide::open_with_options(path, options(cache, mode == "cpu")).unwrap();
            let start = Instant::now();
            let actual = if callers == 1 {
                vec![slide.read_tiles(&requests).unwrap()]
            } else {
                let barrier = std::sync::Barrier::new(callers);
                std::thread::scope(|scope| {
                    let workers = (0..callers)
                        .map(|_| {
                            scope.spawn(|| {
                                barrier.wait();
                                slide.read_tiles(&requests).unwrap()
                            })
                        })
                        .collect::<Vec<_>>();
                    workers
                        .into_iter()
                        .map(|worker| worker.join().unwrap())
                        .collect()
                })
            };
            times.push(start.elapsed().as_secs_f64() * 1e6);
            for tiles in actual {
                validate(&tiles, &expected, mode == "cpu");
            }
        }
        results.push(json!({"cache":row["cache"],"level":row["level"],"batch":row["batch"],
            "concurrency":callers,"cycles":times.len(),"requests":row["requests"],
            "mean_sequence_us":times.iter().sum::<f64>() / times.len() as f64,"sequence_times_us":times}));
    }
    assert!(!results.is_empty());
    std::fs::write(std::env::var_os("WSI_RS_DECODE_OUTPUT").expect("output"),
        serde_json::to_vec_pretty(&json!({"source":path,"mode":mode,"results":results,
            "timing":"Fresh readers with process routes warmed; opening and validation excluded; concurrent wall time includes caller startup"})).unwrap()).unwrap();
}
