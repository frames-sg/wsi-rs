use super::*;
use crate::{
    DecodeAcceleration, DecodeExecutionOptions, Slide, SlideOpenOptions, TileLayout, TileRequest,
};

#[cfg(all(feature = "metal", target_os = "macos"))]
#[test]
#[ignore = "explicit release Metal window comparison using WSI_RS_PREPARE_PATH and WSI_RS_PREPARE_OUTPUT"]
fn metal_window_performance() {
    let raw = cpu_corpus_tiles(64);
    let jobs = raw
        .iter()
        .map(|raw| Jp2kDecodeJob {
            data: std::borrow::Cow::Borrowed(raw.data()),
            expected_width: raw.width(),
            expected_height: raw.height(),
            rgb_color_space: raw.photometric_interpretation()
                == crate::EncodedTilePhotometricInterpretation::Rgb,
            backend: BackendRequest::Cpu,
        })
        .collect::<Vec<_>>();
    let expected = super::super::batch::decode_batch_jp2k(&jobs)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut jobs = jobs;
    for job in &mut jobs {
        job.backend = BackendRequest::Metal;
    }
    let mut records = Vec::new();
    for pair in 0..5 {
        let windows = if pair % 2 == 0 {
            [4, 8, 16]
        } else {
            [16, 8, 4]
        };
        for mib in windows {
            let sessions = crate::output::metal::MetalBackendSessions::system_default().unwrap();
            let run = || {
                let tiles = super::super::metal_batch::decode_jobs_bounded(
                    &jobs,
                    &sessions,
                    mib * 1024 * 1024,
                )
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
                sessions.download_cpu_batch(&tiles).unwrap()
            };
            drop(run());
            let start = std::time::Instant::now();
            let actual = run();
            let elapsed = start.elapsed();
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(&expected) {
                assert_eq!(actual.as_u8(), expected.as_u8());
            }
            records.push(serde_json::json!({"window_mib": mib, "pair": pair, "elapsed_ns": elapsed.as_nanos()}));
        }
    }
    std::fs::write(
        std::env::var_os("WSI_RS_PREPARE_OUTPUT").expect("capture path"),
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();
}

#[cfg(all(feature = "metal", target_os = "macos"))]
#[test]
#[ignore = "explicit cooperative Metal comparison using WSI_RS_PREPARE_PATH and WSI_RS_PREPARE_OUTPUT"]
fn metal_cooperative_performance() {
    use crate::core::execution_telemetry::{test_count, Event};
    use crate::output::metal::MetalBackendSessions;
    use std::num::NonZeroUsize;

    let raw = cpu_corpus_tiles(16);
    let mut jobs = raw
        .iter()
        .map(|raw| Jp2kDecodeJob {
            data: std::borrow::Cow::Borrowed(raw.data()),
            expected_width: raw.width(),
            expected_height: raw.height(),
            rgb_color_space: raw.photometric_interpretation()
                == crate::EncodedTilePhotometricInterpretation::Rgb,
            backend: BackendRequest::Cpu,
        })
        .collect::<Vec<_>>();
    let expected = super::super::batch::decode_batch_jp2k(&jobs)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for job in &mut jobs {
        job.backend = BackendRequest::Metal;
    }
    let pool_workers = crate::core::decode_runtime::DecodeRuntime::default_arc().cpu_worker_count();
    let mut budgets = vec![1, 2, 4, pool_workers];
    budgets.sort_unstable();
    budgets.dedup();
    let mut records = Vec::new();
    for count in [4, 8, 16] {
        for window_mib in [4, 8, 16] {
            for &workers in &budgets {
                let gpu = MetalBackendSessions::system_default().expect("live Metal device");
                let cooperative = gpu
                    .clone()
                    .with_cooperative_cpu_workers(NonZeroUsize::new(workers).unwrap());
                let run = |session| {
                    let tiles = super::super::metal_batch::decode_jobs_bounded(
                        &jobs[..count],
                        session,
                        window_mib * 1024 * 1024,
                    )
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                    session.download_cpu_batch(&tiles).unwrap()
                };
                let verify = |actual: &[CpuTile]| {
                    assert_eq!(actual.len(), count);
                    for (actual, expected) in actual.iter().zip(&expected[..count]) {
                        assert_eq!(
                            (actual.width(), actual.height()),
                            (expected.width(), expected.height())
                        );
                        assert_eq!(actual.as_u8(), expected.as_u8());
                    }
                };
                // Warm both paths before measuring planning, decoding and readback.
                verify(&run(&gpu));
                verify(&run(&cooperative));
                for pair in 0..5 {
                    for enabled in [pair % 2 == 1, pair % 2 == 0] {
                        let session = if enabled { &cooperative } else { &gpu };
                        let before = test_count(Event::MetalCpuTier1Images);
                        let start = std::time::Instant::now();
                        let actual = run(session);
                        let elapsed = start.elapsed();
                        let cpu_images = test_count(Event::MetalCpuTier1Images) - before;
                        verify(&actual);
                        records.push(serde_json::json!({
                            "batch": count,
                            "window_mib": window_mib,
                            "cpu_workers_requested": workers,
                            "wsi_pool_workers": pool_workers,
                            "cooperative": enabled,
                            "cpu_tier1_images": cpu_images,
                            "pair": pair,
                            "elapsed_ns": elapsed.as_nanos(),
                        }));
                    }
                }
            }
        }
    }
    let report = serde_json::json!({
        "slide": std::env::var_os("WSI_RS_PREPARE_PATH").unwrap().to_string_lossy(),
        "includes": ["input preparation", "decode", "CPU readback"],
        "cooperative_was_exercised": records.iter().any(|row| row["cpu_tier1_images"].as_u64().unwrap() > 0),
        "runs": records,
    });
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(std::env::var_os("WSI_RS_PREPARE_OUTPUT").expect("capture path"))
        .expect("capture path must be new");
    serde_json::to_writer_pretty(output, &report).unwrap();
}

#[test]
#[ignore = "explicit release benchmark using WSI_RS_PREPARE_PATH and WSI_RS_PREPARE_OUTPUT"]
fn prepared_cpu_performance() {
    let raw = cpu_corpus_tiles(64);
    let jobs = raw
        .iter()
        .map(|raw| Jp2kDecodeJob {
            data: std::borrow::Cow::Borrowed(raw.data()),
            expected_width: raw.width(),
            expected_height: raw.height(),
            rgb_color_space: raw.photometric_interpretation()
                == crate::EncodedTilePhotometricInterpretation::Rgb,
            backend: BackendRequest::Cpu,
        })
        .collect::<Vec<_>>();
    let runtime = crate::core::decode_runtime::DecodeRuntime::default_arc();
    let mut results = Vec::new();
    runtime.install_jp2k_cpu(|| {
        for count in [1, 4, 8, 16, 64] {
            let jobs = &jobs[..count];
            let expected = super::super::batch::decode_batch_jp2k(jobs).into_iter().collect::<Result<Vec<_>, _>>().unwrap();
            let prepared = PreparedJp2kBatch::new(jobs, runtime.cpu_worker_count()).unwrap();
            let actual = prepared.read_cpu().unwrap();
            assert_eq!(actual.len(), expected.len());
            for (a, b) in actual.iter().zip(&expected) { assert!(a.as_u8() == b.as_u8(), "prepared CPU output differs from established CPU bytes"); }
            for pair in 0..5 {
                for candidate in [pair % 2 == 1, pair % 2 == 0] {
                    let start = std::time::Instant::now();
                    let output = if candidate {
                        PreparedJp2kBatch::new(jobs, runtime.cpu_worker_count()).unwrap().read_cpu().unwrap()
                    } else { super::super::batch::decode_batch_jp2k(jobs).into_iter().collect::<Result<Vec<_>, _>>().unwrap() };
                    let elapsed = start.elapsed();
                    assert_eq!(output.len(), expected.len());
                    for (a, b) in output.iter().zip(&expected) { assert!(a.as_u8() == b.as_u8(), "prepared CPU output differs from established CPU bytes"); }
                    results.push(serde_json::json!({"batch":count,"pair":pair,"candidate":candidate,"elapsed_ns":elapsed.as_nanos()}));
                }
            }
        }
    });
    std::fs::write(
        std::env::var_os("WSI_RS_PREPARE_OUTPUT").expect("capture path"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
}

fn cpu_corpus_tiles(count: u64) -> Vec<crate::RawCompressedTile> {
    let path = std::env::var_os("WSI_RS_PREPARE_PATH").expect("corpus path");
    let slide = Slide::open_with_options(
        path,
        SlideOpenOptions::default().with_decode_execution_options(
            DecodeExecutionOptions::default().with_acceleration(DecodeAcceleration::CpuOnly),
        ),
    )
    .unwrap();
    let TileLayout::Regular {
        tiles_across,
        tiles_down,
        ..
    } = slide.dataset().scenes[0].series[0].levels[0].tile_layout
    else {
        panic!("regular tiles required")
    };
    (0..count)
        .map(|i| {
            slide
                .read_raw_compressed_tile(&TileRequest::new(
                    0,
                    0,
                    0,
                    ((tiles_across / 2 + i % 8) % tiles_across) as i64,
                    ((tiles_down / 2 + i / 8) % tiles_down) as i64,
                ))
                .unwrap()
        })
        .collect::<Vec<_>>()
}

#[test]
#[ignore = "explicit release CPU scheduling comparison using WSI_RS_PREPARE_PATH and WSI_RS_PREPARE_OUTPUT"]
fn pooled_cpu_scheduling_performance() {
    use rayon::prelude::*;
    let individual = std::env::var_os("WSI_RS_PREPARE_INDIVIDUAL").is_some();
    let raw = cpu_corpus_tiles(64);
    let metadata = raw
        .iter()
        .map(|raw| {
            super::super::prepare::prepare_jp2k_input(
                raw.data(),
                raw.width(),
                raw.height(),
                Jp2kColorSpace::Rgb,
                BackendRequest::Cpu,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let runtime = crate::core::decode_runtime::DecodeRuntime::default_arc();
    let mut records = Vec::new();
    runtime.install_jp2k_cpu(|| {
        for count in [1, 4, 8, 16, 64] {
            let metadata = &metadata[..count];
            let run = |pooled| {
                let mut outputs = metadata.iter().map(|job| vec![0_u8; job.output_len]).collect::<Vec<_>>();
                let mut jobs = metadata.iter().zip(&mut outputs).map(|(job, output)| j2k::TileDecodeJob {
                    input: job.input, out: output.as_mut_slice(), stride: job.row_bytes,
                }).collect::<Vec<_>>();
                let workers = rayon::current_num_threads();
                if pooled && workers > 1 {
                    let chunk = jobs.len().div_ceil(workers.min(4));
                    jobs.par_chunks_mut(chunk).try_for_each(|jobs| {
                        if individual {
                            for job in jobs {
                                let mut decoder = j2k::J2kDecoder::new(job.input).map_err(|error| error.to_string())?;
                                decoder.set_cpu_decode_parallelism(j2k::CpuDecodeParallelism::Serial);
                                decoder.decode_into(job.out, job.stride, j2k_core::PixelFormat::Rgb8).map_err(|error| error.to_string())?;
                            }
                            Ok(())
                        } else {
                            j2k::decode_tiles_into(jobs, j2k_core::PixelFormat::Rgb8,
                                j2k::TileBatchOptions { workers: std::num::NonZeroUsize::new(1) }).map(|_| ()).map_err(|error| error.to_string())
                        }
                    }).unwrap();
                } else {
                    j2k::decode_tiles_into(&mut jobs, j2k_core::PixelFormat::Rgb8,
                        j2k::TileBatchOptions { workers: std::num::NonZeroUsize::new(workers) }).unwrap();
                }
                outputs
            };
            let expected = run(false);
            assert!(run(true) == expected, "pooled borrowed CPU bytes differ at {count} tiles");
            for pair in 0..5 {
                for pooled in [pair % 2 == 1, pair % 2 == 0] {
                    let start = std::time::Instant::now();
                    let actual = run(pooled);
                    let elapsed = start.elapsed();
                    assert!(actual == expected, "pooled borrowed CPU bytes differ at {count} tiles");
                    records.push(serde_json::json!({"batch":count,"pair":pair,"pooled":pooled,"individual":individual,"elapsed_ns":elapsed.as_nanos()}));
                }
            }
        }
    });
    std::fs::write(
        std::env::var_os("WSI_RS_PREPARE_OUTPUT").unwrap(),
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();
}
