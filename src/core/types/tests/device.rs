use super::super::*;

#[test]
fn tile_output_preference_constructors_map_correctly() {
    match TileOutputPreference::cpu() {
        TileOutputPreference::Cpu { backend } => {
            assert!(matches!(backend, OutputBackendRequest::Auto));
        }
        other => panic!("cpu() must produce Cpu/Auto, got {other:?}"),
    }
    match TileOutputPreference::cpu_only() {
        TileOutputPreference::Cpu { backend } => {
            assert!(matches!(backend, OutputBackendRequest::Cpu));
        }
        other => panic!("cpu_only() must produce Cpu/Cpu, got {other:?}"),
    }
    assert!(matches!(
        TileOutputPreference::prefer_device_auto(),
        TileOutputPreference::PreferDevice {
            backend: OutputBackendRequest::Auto,
            ..
        }
    ));
    assert!(matches!(
        TileOutputPreference::require_device_auto(),
        TileOutputPreference::RequireDevice {
            backend: OutputBackendRequest::Auto,
            ..
        }
    ));
    #[cfg(feature = "metal")]
    assert!(matches!(
        TileOutputPreference::require_metal(),
        TileOutputPreference::RequireDevice {
            backend: OutputBackendRequest::Metal,
            ..
        }
    ));
    #[cfg(feature = "cuda")]
    assert!(matches!(
        TileOutputPreference::require_cuda(),
        TileOutputPreference::RequireDevice {
            backend: OutputBackendRequest::Cuda,
            ..
        }
    ));
}

#[test]
fn tile_output_preference_compressed_device_decode_is_explicit() {
    assert!(!TileOutputPreference::prefer_device_auto().compressed_device_decode_enabled());
    assert!(
        TileOutputPreference::prefer_device_auto_with_compressed_decode()
            .compressed_device_decode_enabled()
    );
    assert!(
        TileOutputPreference::require_device_auto_with_compressed_decode()
            .compressed_device_decode_enabled()
    );
    assert!(!TileOutputPreference::require_device_auto().compressed_device_decode_enabled());
    assert!(TileOutputPreference::require_device_auto().requires_device());
    assert!(TileOutputPreference::require_device_auto_with_compressed_decode().requires_device());
}

#[test]
fn tile_output_preference_can_disable_adaptive_decode_route() {
    let preference = TileOutputPreference::prefer_device_auto_with_compressed_decode();
    assert!(preference.adaptive_decode_route_enabled());

    let preference = preference.without_adaptive_decode_route();
    assert!(!preference.adaptive_decode_route_enabled());
    assert!(preference.compressed_device_decode_enabled());
}

#[cfg(feature = "metal")]
#[test]
fn tile_output_preference_metal_constructors_attach_sessions() {
    let Ok(sessions) = crate::output::metal::MetalBackendSessions::system_default() else {
        return;
    };

    match TileOutputPreference::prefer_device_auto_with_metal(sessions.clone()) {
        TileOutputPreference::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context,
        } => {
            assert!(context.metal().is_some());
            assert!(!context.compressed_device_decode());
        }
        other => {
            panic!("prefer_device_auto_with_metal must produce PreferDevice/Auto, got {other:?}")
        }
    }

    match TileOutputPreference::prefer_device_auto_with_metal_and_compressed_decode(
        sessions.clone(),
    ) {
        TileOutputPreference::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context,
        } => {
            assert!(context.metal().is_some());
            assert!(context.compressed_device_decode());
        }
        other => panic!(
            "prefer_device_auto_with_metal_and_compressed_decode must produce PreferDevice/Auto, got {other:?}"
        ),
    }

    match TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(sessions) {
        TileOutputPreference::RequireDevice {
            backend: OutputBackendRequest::Auto,
            context,
        } => {
            assert!(context.metal().is_some());
            assert!(context.compressed_device_decode());
        }
        other => panic!(
            "require_device_auto_with_metal_and_compressed_decode must produce RequireDevice/Auto, got {other:?}"
        ),
    }

    assert_eq!(
        OutputBackendRequest::Metal.to_j2k(),
        j2k_core::BackendRequest::Metal
    );
}

#[cfg(feature = "metal")]
#[test]
fn device_output_context_holds_metal_sessions() {
    let Ok(sessions) = crate::output::metal::MetalBackendSessions::system_default() else {
        return;
    };
    let context = DeviceOutputContext::with_metal(sessions);

    assert!(context.metal().is_some());
    assert!(!context.compressed_device_decode());
    assert!(context.adaptive_decode_route());
}

#[cfg(feature = "metal")]
#[test]
fn metal_output_types_are_clone_debug_surfaces() {
    fn assert_clone_debug<T: Clone + std::fmt::Debug>() {}

    assert_clone_debug::<crate::output::metal::MetalBackendSessions>();
    assert_clone_debug::<crate::output::metal::MetalDeviceStorage>();
    assert_clone_debug::<crate::output::metal::MetalDeviceTile>();
}

#[cfg(feature = "cuda")]
#[test]
fn tile_output_preference_cuda_constructors_attach_sessions() {
    let sessions = crate::output::cuda::CudaBackendSessions::new();

    match TileOutputPreference::prefer_device_auto_with_cuda(sessions.clone()) {
        TileOutputPreference::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context,
        } => {
            assert!(context.cuda().is_some());
            assert!(!context.compressed_device_decode());
        }
        other => {
            panic!("prefer_device_auto_with_cuda must produce PreferDevice/Auto, got {other:?}")
        }
    }

    match TileOutputPreference::prefer_device_auto_with_cuda_and_compressed_decode(
        sessions.clone(),
    ) {
        TileOutputPreference::PreferDevice {
            backend: OutputBackendRequest::Auto,
            context,
        } => {
            assert!(context.cuda().is_some());
            assert!(context.compressed_device_decode());
        }
        other => panic!(
            "prefer_device_auto_with_cuda_and_compressed_decode must produce PreferDevice/Auto, got {other:?}"
        ),
    }

    match TileOutputPreference::require_device_auto_with_cuda_and_compressed_decode(sessions) {
        TileOutputPreference::RequireDevice {
            backend: OutputBackendRequest::Auto,
            context,
        } => {
            assert!(context.cuda().is_some());
            assert!(context.compressed_device_decode());
        }
        other => panic!(
            "require_device_auto_with_cuda_and_compressed_decode must produce RequireDevice/Auto, got {other:?}"
        ),
    }
}

#[cfg(feature = "cuda")]
#[test]
fn device_output_context_holds_cuda_sessions() {
    let sessions = crate::output::cuda::CudaBackendSessions::new();
    let context = DeviceOutputContext::with_cuda(sessions);

    assert!(context.cuda().is_some());
    assert!(!context.compressed_device_decode());
    assert!(context.adaptive_decode_route());
}

#[cfg(feature = "cuda")]
#[test]
fn cuda_output_types_are_clone_debug_surfaces() {
    fn assert_clone_debug<T: Clone + std::fmt::Debug>() {}

    assert_clone_debug::<crate::output::cuda::CudaBackendSessions>();
    assert_clone_debug::<crate::output::cuda::CudaDeviceStorage>();
    assert_clone_debug::<crate::output::cuda::CudaDeviceTile>();
}

// --- TileLayout intersection ---
