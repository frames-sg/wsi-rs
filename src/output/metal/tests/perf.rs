use std::time::{Duration, Instant};

use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandEncoder, MTLComputeCommandEncoder, MTLComputePipelineState};

use super::*;

#[test]
#[ignore = "run explicitly in release mode for the five-run Metal address-width gate"]
fn ycbcr_selected_u32_stays_within_five_percent_of_reference() {
    const DIMENSION: u32 = 2_048;
    // Millisecond-scale samples varied by 40% even between the two equivalent
    // u32 kernels. Amortize submission jitter without changing the 5% ceiling.
    const DISPATCHES_PER_SAMPLE: usize = 512;
    const SAMPLE_COUNT: usize = 5;

    let device = test_device().expect("the explicit address-width gate requires Metal");
    let source = format!(
        "{YCBCR_TO_RGB8_METAL}\n{}",
        include_str!("../ycbcr_perf.metal")
    );
    let loader = j2k_metal_support::MetalPipelineLoader::new(&device, &source)
        .expect("compile YCbCr address performance kernels");
    let pipeline = |name| {
        loader
            .pipeline(name)
            .expect("create YCbCr address pipeline")
    };
    let reference_pipeline = pipeline("wsi_rs_ycbcr8_to_rgb8_u32_perf_reference");
    let selected_u32_pipeline = pipeline("wsi_rs_ycbcr8_to_rgb8_u32");
    let u64_pipeline = pipeline("wsi_rs_ycbcr8_to_rgb8");
    let pitch = DIMENSION * 3;
    let byte_len = usize::try_from(pitch)
        .expect("pitch fits usize")
        .checked_mul(usize::try_from(DIMENSION).expect("height fits usize"))
        .expect("performance buffer length");
    let pixels: Vec<u8> = (0..byte_len)
        .map(|i| (i * 73 + i / 256 * 17) as u8)
        .collect();
    let src = j2k_metal_support::checked_shared_buffer_with_slice(&device, &pixels)
        .expect("allocate performance source");
    let dst = j2k_metal_support::checked_shared_buffer_for_len::<u8>(&device, byte_len)
        .expect("allocate performance destination");
    let params = YcbcrToRgb8Params {
        width: DIMENSION,
        height: DIMENSION,
        src_pitch: pitch,
        dst_pitch: pitch,
    };
    let queue = j2k_metal_support::checked_command_queue(&device)
        .expect("create performance command queue");
    let tables = j2k_metal_support::checked_shared_buffer_with_slice(
        &device,
        &crate::decode::jp2k_raster::ycbcr_shader_tables(),
    )
    .unwrap();

    let measure = |pipeline: &ProtocolObject<dyn MTLComputePipelineState>, dispatches: usize| {
        let command_buffer = j2k_metal_support::checked_command_buffer(&queue)
            .expect("create performance command buffer");
        let started = Instant::now();
        for _ in 0..dispatches {
            let encoder = j2k_metal_support::checked_compute_command_encoder(&command_buffer)
                .expect("create performance encoder");
            encoder.setComputePipelineState(pipeline);
            interop::bind_compute_buffer(&encoder, 0, &src);
            interop::bind_compute_buffer(&encoder, 1, &dst);
            interop::bind_ycbcr_params(&encoder, 2, &params);
            interop::bind_compute_buffer(&encoder, 3, &tables);
            j2k_metal_support::dispatch_2d_pipeline(&encoder, pipeline, (DIMENSION, DIMENSION));
            encoder.endEncoding();
        }
        j2k_metal_support::commit_and_wait(&command_buffer)
            .expect("complete YCbCr address performance sample");
        started.elapsed()
    };

    let expected = crate::decode::jp2k_raster::interleaved_image_to_sample_buffer(
        crate::decode::jp2k_backend::DecodedInterleavedImage {
            width: DIMENSION as usize,
            height: DIMENSION as usize,
            colorspace: crate::decode::jp2k::Jp2kColorSpace::YCbCr,
            pixels,
        },
    )
    .unwrap();
    for pipeline in [&reference_pipeline, &selected_u32_pipeline, &u64_pipeline] {
        measure(pipeline, 32);
        let actual: Vec<u8> = u64_buffer_values(&dst, byte_len / 8)
            .into_iter()
            .flat_map(u64::to_ne_bytes)
            .collect();
        assert!(
            actual.as_slice() == expected.as_u8().unwrap(),
            "performance kernel must match CPU pixels before timing"
        );
    }
    let mut reference_samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut selected_u32_samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut u64_samples = Vec::with_capacity(SAMPLE_COUNT);
    for sample in 0..SAMPLE_COUNT {
        if sample % 2 == 0 {
            reference_samples.push(measure(&reference_pipeline, DISPATCHES_PER_SAMPLE));
            selected_u32_samples.push(measure(&selected_u32_pipeline, DISPATCHES_PER_SAMPLE));
            u64_samples.push(measure(&u64_pipeline, DISPATCHES_PER_SAMPLE));
        } else {
            u64_samples.push(measure(&u64_pipeline, DISPATCHES_PER_SAMPLE));
            selected_u32_samples.push(measure(&selected_u32_pipeline, DISPATCHES_PER_SAMPLE));
            reference_samples.push(measure(&reference_pipeline, DISPATCHES_PER_SAMPLE));
        }
    }

    let median = |samples: &mut Vec<Duration>| {
        samples.sort_unstable();
        samples[samples.len() / 2]
    };
    let reference_median = median(&mut reference_samples);
    let selected_u32_median = median(&mut selected_u32_samples);
    let u64_median = median(&mut u64_samples);
    let selected_ratio = selected_u32_median.as_secs_f64() / reference_median.as_secs_f64();
    let u64_ratio = u64_median.as_secs_f64() / reference_median.as_secs_f64();
    eprintln!(
        "Metal YCbCr address-width benchmark: reference={reference_median:?} selected_u32={selected_u32_median:?} u64={u64_median:?} selected_ratio={selected_ratio:.4} u64_ratio={u64_ratio:.4}"
    );
    assert!(
        selected_ratio <= 1.05,
        "selected u32 Metal YCbCr path regressed by more than 5%: ratio={selected_ratio:.4}"
    );
}
