use super::*;
use crate::{
    DecodeAcceleration, DecodeExecutionOptions, Slide, SlideOpenOptions, TileLayout, TileRequest,
};

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
            for (a, b) in actual.iter().zip(&expected) { assert!(a.as_u8() == b.as_u8(), "prepared CPU output differs from established CPU bytes"); }
            for pair in 0..5 {
                for candidate in [pair % 2 == 1, pair % 2 == 0] {
                    let start = std::time::Instant::now();
                    let output = if candidate {
                        PreparedJp2kBatch::new(jobs, runtime.cpu_worker_count()).unwrap().read_cpu().unwrap()
                    } else { super::super::batch::decode_batch_jp2k(jobs).into_iter().collect::<Result<Vec<_>, _>>().unwrap() };
                    let elapsed = start.elapsed();
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

#[test]
#[ignore = "explicit release experiment using WSI_RS_PREPARE_PATH and WSI_RS_PREPARE_OUTPUT"]
fn two_tile_cpu_scheduling_performance() {
    use rayon::prelude::*;
    let raw = cpu_corpus_tiles(8);
    let jobs = raw
        .iter()
        .map(|r| Jp2kDecodeJob {
            data: std::borrow::Cow::Borrowed(r.data()),
            expected_width: r.width(),
            expected_height: r.height(),
            rgb_color_space: r.photometric_interpretation()
                == crate::EncodedTilePhotometricInterpretation::Rgb,
            backend: BackendRequest::Cpu,
        })
        .collect::<Vec<_>>();
    let runtime = crate::core::decode_runtime::DecodeRuntime::default_arc();
    let mut records = Vec::new();
    runtime.install_jp2k_cpu(|| {
        for jobs in jobs.chunks_exact(2) {
            let run = |candidate| {
                if candidate { jobs.par_iter().map(|job| super::super::cpu::decode_one_jp2k_job_with_parallelism(job,j2k::CpuDecodeParallelism::Serial)).collect::<Result<Vec<_>,_>>().unwrap() }
                else { super::super::batch::try_decode_batch_jp2k_with_j2k(jobs).unwrap() }
            };
            let expected=run(false);
            for (a,b) in run(true).iter().zip(&expected) { assert!(a.as_u8()==b.as_u8(),"short batch CPU pixels differ"); }
            for pair in 0..5 { for candidate in [pair%2==1,pair%2==0] {
                let start=std::time::Instant::now(); let actual=run(candidate); let elapsed=start.elapsed();
                for (a,b) in actual.iter().zip(&expected) { assert!(a.as_u8()==b.as_u8()); }
                records.push(serde_json::json!({"pair":pair,"candidate":candidate,"elapsed_ns":elapsed.as_nanos()}));
            } }
        }
    });
    std::fs::write(
        std::env::var_os("WSI_RS_PREPARE_OUTPUT").unwrap(),
        serde_json::to_vec_pretty(&records).unwrap(),
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
