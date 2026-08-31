#![cfg(all(target_os = "macos", feature = "metal"))]

use j2k_core::{BackendKind, BackendRequest, DeviceSurface, ImageDecodeDevice, PixelFormat};

const _: () = {
    fn assert_send<T: Send>() {}
    let _ = assert_send::<j2k_metal::Surface>;
};

#[test]
fn metal_surface_accessors_are_public_for_j2k() {
    let j2k_bytes = fixture_gray8_j2k();
    let mut j2k_decoder = j2k_metal::J2kDecoder::new(&j2k_bytes).expect("j2k decoder");
    let j2k_surface = j2k_decoder
        .decode_to_device(PixelFormat::Gray8, BackendRequest::Metal)
        .expect("j2k metal surface");
    describe_j2k_surface("j2k-metal", &j2k_surface);
    let j2k_image = j2k_surface
        .resident_metal_image()
        .expect("J2K resident Metal image");
    assert_eq!(j2k_surface.backend_kind(), BackendKind::Metal);
    assert_eq!(j2k_image.byte_offset(), 0);
    assert!(j2k_image.byte_len() >= j2k_surface.byte_len());

    let mut j2k_cpu_decoder = j2k_metal::J2kDecoder::new(&j2k_bytes).expect("j2k cpu decoder");
    let j2k_cpu_surface = j2k_cpu_decoder
        .decode_to_device(PixelFormat::Gray8, BackendRequest::Cpu)
        .expect("j2k cpu surface");
    describe_j2k_surface("j2k-cpu", &j2k_cpu_surface);
    assert_eq!(j2k_cpu_surface.backend_kind(), BackendKind::Cpu);
    assert!(j2k_cpu_surface.resident_metal_image().is_none());
}

fn describe_j2k_surface(label: &str, surface: &j2k_metal::Surface) {
    let resident = surface
        .resident_metal_image()
        .map(|image| (image.device_registry_id(), image.byte_offset()));
    println!(
        "{label}: dimensions={:?} pitch_bytes={} pixel_format={:?} backend={:?} resident={resident:?}",
        surface.dimensions(),
        surface.pitch_bytes(),
        surface.pixel_format(),
        surface.backend_kind(),
    );
}

fn fixture_gray8_j2k() -> Vec<u8> {
    let pixels: Vec<u8> = (0..16).collect();
    let options = j2k_native::EncodeOptions {
        reversible: true,
        num_decomposition_levels: 1,
        ..j2k_native::EncodeOptions::default()
    };
    j2k_native::encode(&pixels, 4, 4, 1, 8, false, &options).expect("encode gray8 j2k fixture")
}
