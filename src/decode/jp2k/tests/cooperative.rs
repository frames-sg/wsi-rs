use std::{borrow::Cow, num::NonZeroUsize};

use super::*;
use crate::core::execution_telemetry::{test_count, Event};
use crate::output::metal::MetalBackendSessions;

fn distinct_jobs() -> Vec<Jp2kDecodeJob<'static>> {
    (0..12_usize)
        .map(|seed| {
            let pixels = (0..128 * 128 * 3)
                .map(|index| ((index * (seed + 3) + index / 128) & 255) as u8)
                .collect::<Vec<_>>();
            let mut options = j2k::J2kLosslessEncodeOptions::default();
            options.max_decomposition_levels = Some(3);
            let encoded = j2k::encode_j2k_lossless(
                j2k::J2kLosslessSamples {
                    data: &pixels,
                    width: 128,
                    height: 128,
                    components: 3,
                    bit_depth: 8,
                    signed: false,
                },
                &options,
            )
            .expect("encode distinct reversible classic RGB tiles");
            Jp2kDecodeJob {
                data: Cow::Owned(encoded.codestream),
                expected_width: 128 - (seed % 3) as u32,
                expected_height: 128 - (seed % 2) as u32,
                rgb_color_space: true,
                backend: j2k_core::BackendRequest::Metal,
            }
        })
        .collect()
}

fn sessions() -> MetalBackendSessions {
    let workers = crate::core::decode_runtime::DecodeRuntime::default_arc().cpu_worker_count();
    assert!(
        workers >= 2,
        "cooperative tests require at least two WSI CPU workers"
    );
    MetalBackendSessions::system_default()
        .expect("cooperative tests require a live Metal device")
        .with_cooperative_cpu_workers(NonZeroUsize::new(workers).unwrap())
}

fn cpu_results(jobs: &[Jp2kDecodeJob<'_>]) -> Vec<Result<CpuTile, WsiError>> {
    decode_batch_jp2k(
        &jobs
            .iter()
            .cloned()
            .map(|mut job| {
                job.backend = j2k_core::BackendRequest::Cpu;
                job
            })
            .collect::<Vec<_>>(),
    )
}

fn assert_results(
    actual: Vec<Result<crate::output::metal::MetalDeviceTile, WsiError>>,
    expected: &[Result<CpuTile, WsiError>],
) {
    assert_eq!(actual.len(), expected.len());
    for (index, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
        match (actual, expected) {
            (Ok(actual), Ok(expected)) => {
                assert_eq!(
                    (actual.width, actual.height),
                    (expected.width(), expected.height())
                );
                assert_eq!(
                    actual.download_cpu().unwrap().as_u8(),
                    expected.as_u8(),
                    "slot {index}"
                );
            }
            (Err(_), Err(_)) => {}
            (actual, expected) => panic!("slot {index}: {actual:?} versus {expected:?}"),
        }
    }
}

#[test]
fn cooperative_metal_preserves_pixels_crops_errors_and_prepared_reuse() {
    let sessions = sessions();
    let jobs = distinct_jobs();
    let expected = cpu_results(&jobs);
    let mut with_error = jobs.clone();
    let mut invalid = jobs[0].clone();
    invalid.data = Cow::Borrowed(b"invalid codestream");
    with_error.insert(5, invalid);
    let before = test_count(Event::MetalCpuTier1Images);
    assert_results(
        decode_batch_jp2k_metal(&with_error, &sessions),
        &cpu_results(&with_error),
    );
    assert!(
        test_count(Event::MetalCpuTier1Images) > before,
        "must exercise CPU entropy decoding"
    );

    let prepared = PreparedJp2kBatch::new(&jobs, 4).unwrap();
    for _ in 0..2 {
        let before = test_count(Event::MetalCpuTier1Images);
        let actual = prepared.read_metal(&sessions).unwrap();
        assert!(
            test_count(Event::MetalCpuTier1Images) > before,
            "prepared reads must cooperate too"
        );
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(&expected) {
            let expected = expected.as_ref().unwrap();
            assert_eq!(
                (actual.width(), actual.height()),
                (expected.width(), expected.height())
            );
            assert_eq!(actual.as_u8(), expected.as_u8());
        }
    }
}

#[test]
fn cooperative_metal_keeps_gpu_fallbacks_and_default_behavior() {
    let sessions = sessions();
    let jobs = distinct_jobs();
    let expected = cpu_results(&jobs);
    let gpu_only = MetalBackendSessions::system_default().unwrap();
    let one_worker = sessions
        .clone()
        .with_cooperative_cpu_workers(NonZeroUsize::new(1).unwrap());
    for session in [&gpu_only, &one_worker] {
        let before = test_count(Event::MetalCpuTier1Images);
        assert_results(decode_batch_jp2k_metal(&jobs, session), &expected);
        assert_eq!(test_count(Event::MetalCpuTier1Images), before);
    }
    let repeated = vec![&jobs[0]; 12]
        .into_iter()
        .map(|job| Jp2kDecodeJob {
            data: Cow::Borrowed(job.data.as_ref()),
            ..job.clone()
        })
        .collect::<Vec<_>>();
    for ineligible in [&jobs[..1], &jobs[..4], repeated.as_slice()] {
        let before = test_count(Event::MetalCpuTier1Images);
        assert_results(
            decode_batch_jp2k_metal(ineligible, &sessions),
            &cpu_results(ineligible),
        );
        assert_eq!(
            test_count(Event::MetalCpuTier1Images),
            before,
            "ineligible groups remain GPU-only"
        );
    }
}

#[test]
fn cooperative_metal_clones_fall_back_while_the_cpu_budget_is_busy() {
    let sessions = sessions();
    let jobs = distinct_jobs();
    let expected = cpu_results(&jobs);
    let peer = sessions.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let holder = scope.spawn(move || {
            let _slot = peer
                .try_cooperative_decode()
                .expect("claim shared CPU budget");
            ready_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("concurrent read must not wait for the CPU budget");
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        let before = test_count(Event::MetalCpuTier1Images);
        let actual = decode_batch_jp2k_metal(&jobs, &sessions);
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(test_count(Event::MetalCpuTier1Images), before);
        assert_results(actual, &expected);
    });
    let before = test_count(Event::MetalCpuTier1Images);
    assert_results(decode_batch_jp2k_metal(&jobs, &sessions), &expected);
    assert!(
        test_count(Event::MetalCpuTier1Images) > before,
        "CPU budget is reusable after release"
    );
}
