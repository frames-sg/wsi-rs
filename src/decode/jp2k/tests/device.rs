use std::borrow::Cow;

use j2k_core::BackendRequest as J2kBackendRequest;

use super::*;

fn rgb_job(backend: J2kBackendRequest) -> Jp2kDecodeJob<'static> {
    let codestream = include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");
    let header = parse_codestream_header(codestream).expect("fixture header");
    Jp2kDecodeJob {
        data: Cow::Borrowed(codestream),
        expected_width: header.image_width,
        expected_height: header.image_height,
        rgb_color_space: true,
        backend,
    }
}

#[cfg(feature = "metal")]
fn metal_sessions() -> Option<crate::output::metal::MetalBackendSessions> {
    crate::output::metal::MetalBackendSessions::system_default().ok()
}

#[cfg(feature = "metal")]
#[test]
fn strict_metal_decode_returns_resident_tile_with_cpu_download_parity() {
    let Some(sessions) = metal_sessions() else {
        eprintln!("skipping JP2K Metal test: no Metal device");
        return;
    };
    let job = rgb_job(J2kBackendRequest::Metal);
    let expected = decode_batch_jp2k(&[rgb_job(J2kBackendRequest::Cpu)])
        .pop()
        .expect("CPU result")
        .expect("CPU decode");

    let tile = decode_one_jp2k_metal(&job, &sessions).expect("strict Metal decode");
    assert_eq!(
        (tile.width, tile.height),
        (job.expected_width, job.expected_height)
    );
    assert_eq!(tile.format, PixelFormat::Rgb8);
    let downloaded = tile.download_cpu().expect("Metal readback");
    assert_eq!(downloaded.as_u8(), expected.as_u8());
}

#[cfg(feature = "metal")]
#[test]
fn strict_metal_batch_preserves_order_cardinality_and_logical_geometry() {
    let Some(sessions) = metal_sessions() else {
        return;
    };
    let full = rgb_job(J2kBackendRequest::Metal);
    let mut cropped = rgb_job(J2kBackendRequest::Metal);
    cropped.expected_width -= 1;
    cropped.expected_height -= 2;
    let expected = [
        (cropped.expected_width, cropped.expected_height),
        (full.expected_width, full.expected_height),
    ];
    let decoded = decode_batch_jp2k_metal(&[cropped, full], &sessions);

    assert_eq!(decoded.len(), expected.len());
    for (decoded, dimensions) in decoded.into_iter().zip(expected) {
        let tile = decoded.expect("strict Metal batch tile");
        assert_eq!((tile.width, tile.height), dimensions);
        assert_eq!(
            tile.download_cpu()
                .expect("cropped readback")
                .as_u8()
                .expect("RGB8 data")
                .len(),
            dimensions.0 as usize * dimensions.1 as usize * 3
        );
    }
}

#[cfg(feature = "metal")]
#[test]
fn strict_metal_groups_duplicate_crops_and_converts_once() {
    use crate::core::execution_telemetry::{test_count, Event};
    let Some(sessions) = metal_sessions() else {
        return;
    };
    let mut a = rgb_job(J2kBackendRequest::Metal);
    a.rgb_color_space = false;
    let mut b = a.clone();
    b.expected_width -= 1;
    b.expected_height -= 2;
    let jobs = [b.clone(), a, b];
    let expected = decode_batch_jp2k(
        &jobs
            .iter()
            .cloned()
            .map(|mut job| {
                job.backend = J2kBackendRequest::Cpu;
                job
            })
            .collect::<Vec<_>>(),
    );
    let submissions = test_count(Event::MetalBatchSubmissions);
    let colors = test_count(Event::ColorSubmissions);
    let groups = test_count(Event::MetalBatchGroups);
    let actual = decode_batch_jp2k_metal(&jobs, &sessions);
    assert_eq!(test_count(Event::MetalBatchSubmissions) - submissions, 1);
    assert_eq!(test_count(Event::ColorSubmissions) - colors, 1);
    assert_eq!(
        test_count(Event::MetalBatchGroups) - groups,
        1,
        "one actual codec group submitted"
    );
    for (actual, expected) in actual.into_iter().zip(expected) {
        let actual = actual.unwrap();
        let expected = expected.unwrap();
        assert_eq!(
            (actual.width, actual.height),
            (expected.width(), expected.height())
        );
        assert_eq!(actual.download_cpu().unwrap().as_u8(), expected.as_u8());
        assert_eq!(
            actual.pitch_bytes,
            expected.width() as usize * 3,
            "color output only allocates logical cropped rows"
        );
    }
}

#[cfg(all(feature = "metal", target_os = "macos"))]
#[test]
fn bounded_metal_groups_keep_duplicate_crops_errors_and_one_color_pass() {
    use crate::core::execution_telemetry::{test_count, Event};
    let Some(sessions) = metal_sessions() else {
        return;
    };
    let mut full = rgb_job(J2kBackendRequest::Metal);
    full.rgb_color_space = false;
    let mut cropped = full.clone();
    cropped.expected_width -= 1;
    cropped.expected_height -= 2;
    let mut invalid = full.clone();
    invalid.data = Cow::Borrowed(b"invalid codestream");
    let jobs = [
        cropped.clone(),
        invalid,
        full.clone(),
        cropped,
        full.clone(),
    ];
    let expected = decode_batch_jp2k(
        &jobs
            .iter()
            .cloned()
            .map(|mut job| {
                job.backend = J2kBackendRequest::Cpu;
                job
            })
            .collect::<Vec<_>>(),
    );
    let target = u64::from(full.expected_width) * u64::from(full.expected_height) * 4 * 2;
    for reuse_prepared in [false, true] {
        let groups = test_count(Event::MetalBatchGroups);
        let colors = test_count(Event::ColorSubmissions);
        let actual = if reuse_prepared {
            let prepared = j2k::prepare_batch(
                jobs.iter()
                    .map(|job| j2k::EncodedImage::full(std::sync::Arc::from(job.data.as_ref())))
                    .collect(),
                j2k::BatchDecodeOptions {
                    layout: j2k::BatchLayout::Nhwc,
                    ..Default::default()
                },
            )
            .unwrap();
            let metadata = jobs
                .iter()
                .map(|job| super::super::prepare::prepare_jp2k_job(job).ok())
                .collect::<Vec<_>>();
            let slots = (0..jobs.len()).collect::<Vec<_>>();
            let mut output = (0..jobs.len()).map(|_| None).collect::<Vec<_>>();
            super::super::metal_batch::execute_prepared_bounded(
                &prepared,
                &slots,
                &metadata,
                &mut output,
                &sessions,
                target,
            )
            .unwrap();
            super::super::metal_batch::convert_outputs(&metadata, &mut output, &sessions);
            output
                .into_iter()
                .map(|result| result.expect("source slot resolved"))
                .collect()
        } else {
            super::super::metal_batch::decode_jobs_bounded(&jobs, &sessions, target)
        };
        assert_eq!(
            test_count(Event::MetalBatchGroups) - groups,
            2,
            "prepared={reuse_prepared}"
        );
        assert_eq!(test_count(Event::ColorSubmissions) - colors, 1);
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.into_iter().zip(&expected) {
            match (actual, expected) {
                (Ok(actual), Ok(expected)) => {
                    assert_eq!(
                        (actual.width, actual.height),
                        (expected.width(), expected.height())
                    );
                    assert_eq!(actual.download_cpu().unwrap().as_u8(), expected.as_u8());
                }
                (Err(_), Err(_)) => {}
                (actual, expected) => panic!("source slot changed: {actual:?} / {expected:?}"),
            }
        }
    }
}

#[cfg(feature = "metal")]
#[test]
fn strict_metal_batch_reports_each_malformed_job_without_cpu_fallback() {
    let Some(sessions) = metal_sessions() else {
        return;
    };
    let malformed = Jp2kDecodeJob {
        data: Cow::Borrowed(b"not a codestream"),
        expected_width: 1,
        expected_height: 1,
        rgb_color_space: true,
        backend: J2kBackendRequest::Metal,
    };
    let results = decode_batch_jp2k_metal(&[malformed], &sessions);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_err());
}

#[cfg(feature = "cuda")]
#[test]
fn strict_cuda_decode_returns_resident_jp2k_tile() {
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    let mut job = rgb_job(J2kBackendRequest::Cuda);
    job.expected_width -= 1;
    job.expected_height -= 2;
    let tile = match decode_one_jp2k_cuda(&job, &sessions) {
        Ok(tile) => tile,
        Err(crate::WsiError::Unsupported { reason })
            if std::env::var_os("J2K_REQUIRE_CUDA_RUNTIME").is_none() =>
        {
            eprintln!("skipping JP2K CUDA test: {reason}");
            return;
        }
        Err(err) => panic!("strict CUDA decode failed: {err}"),
    };
    assert_eq!(
        (tile.width, tile.height),
        (job.expected_width, job.expected_height)
    );
    assert_eq!(tile.format, PixelFormat::Rgb8);
    assert_ne!(tile.storage.device_ptr(), 0);
    assert_eq!(
        tile.storage.j2k_surface().residency(),
        j2k_cuda::SurfaceResidency::CudaResidentDecode
    );
    let downloaded = tile.download_cpu().expect("CUDA readback");
    assert_eq!(
        (downloaded.width(), downloaded.height()),
        (tile.width, tile.height)
    );
    assert_eq!(
        downloaded.as_u8().expect("RGB8 CUDA download").len(),
        tile.width as usize * tile.height as usize * 3
    );
}

#[cfg(feature = "cuda")]
#[test]
fn strict_cuda_batch_preserves_empty_and_result_cardinality() {
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    assert!(decode_batch_jp2k_cuda(&[], &sessions).is_empty());
    let results = decode_batch_jp2k_cuda(
        &[
            rgb_job(J2kBackendRequest::Cuda),
            rgb_job(J2kBackendRequest::Cuda),
        ],
        &sessions,
    );
    assert_eq!(results.len(), 2);
}

#[cfg(feature = "metal")]
#[test]
fn prepared_cpu_and_metal_reuse_inputs_with_duplicate_mixed_crops() {
    let Some(sessions) = metal_sessions() else {
        return;
    };
    let mut a = rgb_job(J2kBackendRequest::Cpu);
    a.rgb_color_space = false;
    let mut b = a.clone();
    b.expected_width -= 1;
    b.expected_height -= 2;
    let jobs = [b.clone(), a, b];
    let oracle = decode_batch_jp2k(&jobs);
    let prepared = crate::decode::jp2k::PreparedJp2kBatch::new(&jobs, 2).unwrap();
    for actual in [
        prepared.read_cpu().unwrap(),
        prepared.read_metal(&sessions).unwrap(),
        prepared.read_cpu().unwrap(),
    ] {
        for (actual, oracle) in actual.iter().zip(&oracle) {
            let oracle = oracle.as_ref().unwrap();
            assert_eq!(
                (actual.width(), actual.height()),
                (oracle.width(), oracle.height())
            );
            assert_eq!(actual.as_u8(), oracle.as_u8());
        }
    }
}

#[cfg(feature = "metal")]
#[test]
fn native_metal_batch_keeps_success_slots_around_a_malformed_input() {
    let Some(sessions) = metal_sessions() else {
        return;
    };
    let a = rgb_job(J2kBackendRequest::Metal);
    let mut b = a.clone();
    b.expected_width -= 1;
    let mut bad = a.clone();
    bad.data = Cow::Borrowed(b"invalid");
    let results = decode_batch_jp2k_metal(&[b.clone(), bad, a.clone(), b], &sessions);
    assert_eq!(results.len(), 4);
    assert!(results[1].is_err());
    assert_eq!(results[0].as_ref().unwrap().width, a.expected_width - 1);
    assert_eq!(results[2].as_ref().unwrap().width, a.expected_width);
    assert_eq!(
        results[0].as_ref().unwrap().download_cpu().unwrap().as_u8(),
        results[3].as_ref().unwrap().download_cpu().unwrap().as_u8()
    );
}

#[cfg(all(feature = "metal", target_os = "macos"))]
#[test]
fn native_metal_windows_bound_image_count_as_well_as_output_bytes() {
    use crate::core::execution_telemetry::{test_count, Event};
    let Some(sessions) = metal_sessions() else {
        return;
    };
    let jobs = vec![rgb_job(J2kBackendRequest::Metal); 17];
    let mut cpu_job = jobs[0].clone();
    cpu_job.backend = J2kBackendRequest::Cpu;
    let expected = decode_batch_jp2k(&[cpu_job]).pop().unwrap().unwrap();
    let mut group_counts = Vec::new();
    for prepared in [false, true] {
        let before = test_count(Event::MetalBatchGroups);
        let actual = if prepared {
            super::super::PreparedJp2kBatch::new(&jobs, 1)
                .unwrap()
                .read_metal(&sessions)
                .unwrap()
        } else {
            decode_batch_jp2k_metal(&jobs, &sessions)
                .into_iter()
                .map(|tile| tile.unwrap().download_cpu().unwrap())
                .collect()
        };
        group_counts.push(test_count(Event::MetalBatchGroups) - before);
        assert_eq!(actual.len(), jobs.len());
        for tile in actual {
            assert_eq!(tile.as_u8(), expected.as_u8());
        }
    }
    assert_eq!(
        group_counts,
        vec![2, 2],
        "both native entry points must bound image count"
    );
}
