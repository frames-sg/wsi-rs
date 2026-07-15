use super::support::*;
use std::fs;

#[test]
fn public_api_snapshot_uses_stable_user_facing_paths() {
    for snapshot_path in [
        "api/wsi-rs-public-api.txt",
        "api/wsi-rs-public-api-cuda.txt",
        "api/wsi-rs-public-api-metal.txt",
    ] {
        let snapshot = fs::read_to_string(crate_root().join(snapshot_path))
            .unwrap_or_else(|err| panic!("read {snapshot_path}: {err}"));

        for forbidden in [
            "wsi_rs::core::",
            "wsi_rs::formats::",
            "CpuTile::new_for_test",
            "CpuTile::solid_red",
            "SlideReadContext<'a>::new",
        ] {
            assert!(
                !snapshot.contains(forbidden),
                "{snapshot_path} must not expose internal/testing detail `{forbidden}`"
            );
        }
    }

    let default_snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api.txt"))
        .expect("read default public API snapshot");
    assert!(
        !default_snapshot.contains("j2k_core::"),
        "default public API snapshot must not expose J2k output-policy internals"
    );
    assert!(
        default_snapshot.contains(
            "pub fn wsi_rs::RawCompressedTile::builder(wsi_rs::Compression) -> wsi_rs::RawCompressedTileBuilder"
        ),
        "Raw compressed tile payloads must expose a named-field builder before the release-candidate API freeze"
    );
    assert!(
        default_snapshot.contains(
            "pub fn wsi_rs::RegionRequest::builder(impl core::convert::Into<wsi_rs::SceneId>, impl core::convert::Into<wsi_rs::SeriesId>, impl core::convert::Into<wsi_rs::LevelIdx>) -> wsi_rs::RegionRequestBuilder"
        ),
        "RegionRequest builders must accept the same typed or plain public indices as tile requests in the public API snapshot"
    );
    assert!(
        !default_snapshot.contains(
            "pub fn wsi_rs::RegionRequest::builder(wsi_rs::SceneId, wsi_rs::SeriesId, wsi_rs::LevelIdx)"
        ),
        "RegionRequest builders must not force manual index newtype construction in the public API snapshot"
    );
    assert!(
        default_snapshot.contains(
            "pub fn wsi_rs::SvcacheTileSelection::new(impl core::convert::Into<wsi_rs::SceneId>, impl core::convert::Into<wsi_rs::SeriesId>, impl core::convert::Into<wsi_rs::LevelIdx>, i64, i64) -> Self"
        ),
        "SvcacheTileSelection must accept the same typed or plain public indices as read requests in the public API snapshot"
    );
    assert!(
        !default_snapshot.contains(
            "pub fn wsi_rs::SvcacheTileSelection::new(wsi_rs::SceneId, wsi_rs::SeriesId, wsi_rs::LevelIdx"
        ),
        "SvcacheTileSelection must not force manual index newtype construction in the public API snapshot"
    );
    assert!(
        default_snapshot.contains(
            "pub fn wsi_rs::Slide::level_source_kind(&self, impl core::convert::Into<wsi_rs::SceneId>, impl core::convert::Into<wsi_rs::SeriesId>, impl core::convert::Into<wsi_rs::LevelIdx>) -> core::result::Result<wsi_rs::LevelSourceKind, wsi_rs::error::WsiError>"
        ),
        "Slide::level_source_kind must accept typed or plain public indices in the public API snapshot"
    );
    assert!(
        default_snapshot.contains(
            "pub fn wsi_rs::SlideReader::level_source_kind(&self, wsi_rs::SceneId, wsi_rs::SeriesId, wsi_rs::LevelIdx)"
        ),
        "SlideReader::level_source_kind must keep object-safe typed index parameters for backend implementations"
    );
    assert!(
        !default_snapshot.contains(
            "pub fn wsi_rs::Slide::level_source_kind(&self, wsi_rs::SceneId, wsi_rs::SeriesId, wsi_rs::LevelIdx)"
        ),
        "Slide::level_source_kind must not force manual index newtype construction in the public API snapshot"
    );
    assert!(
        !default_snapshot.contains("pub fn wsi_rs::RawCompressedTile::new("),
        "Raw compressed tile payloads must not advertise a long positional public constructor before the release-candidate API freeze"
    );

    let metal_snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api-metal.txt"))
        .expect("read Metal public API snapshot");
    assert!(
        metal_snapshot
            .contains("pub wsi_rs::output::metal::MetalDeviceTile::format: wsi_rs::PixelFormat"),
        "Metal device tiles must expose a wsi-rs-owned pixel format type"
    );
    assert!(
        metal_snapshot.contains(
            "pub fn wsi_rs::output::metal::MetalDeviceTile::validated_resident_image(&self) -> core::result::Result<&j2k_metal_support::resident::ResidentMetalImage, wsi_rs::error::WsiError>"
        ),
        "Metal device tiles must expose one canonical resident metadata validation path"
    );
    assert!(
        !metal_snapshot.contains(
            "pub wsi_rs::output::metal::MetalDeviceTile::format: j2k_core::"
        ),
        "Metal device tile payloads must not expose J2k pixel format types as the public field contract"
    );
    assert!(
        metal_snapshot.contains(
            "pub fn wsi_rs::output::metal::MetalBackendSessions::new(metal::device::Device) -> Self"
        ),
        "Metal backend setup should accept a Metal device directly instead of requiring callers to construct codec adapter sessions"
    );
    for forbidden in [
        "j2k_jpeg_metal::MetalBackendSession",
        "j2k_metal::MetalBackendSession",
        "MetalBackendSessions::with_private_jpeg_decode",
    ] {
        assert!(
            !metal_snapshot.contains(forbidden),
            "Metal backend setup must not expose internal codec/session tuning `{forbidden}` in the public API"
        );
    }
}

#[test]
fn default_output_api_keeps_metal_constructors_feature_gated() {
    let snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api.txt"))
        .expect("read public API snapshot");
    let metal_snapshot = fs::read_to_string(crate_root().join("api/wsi-rs-public-api-metal.txt"))
        .expect("read Metal public API snapshot");
    let output = read_repo_text("src/core/types/output.rs");

    assert!(
        snapshot.contains("TileOutputPreference::require_device_auto() -> Self"),
        "default output API must expose a generic require-device constructor"
    );
    assert!(
        !snapshot.contains("TileOutputPreference::require_metal"),
        "default public API must not advertise Metal-specific constructors"
    );
    for forbidden in [
        "pub wsi_rs::OutputBackendRequest::Metal",
        "pub wsi_rs::OutputBackendRequest::Cuda",
    ] {
        assert!(
            !snapshot.contains(forbidden),
            "default public API must not expose feature-specific output backend variant `{forbidden}`"
        );
    }
    assert!(
        metal_snapshot.contains("TileOutputPreference::require_metal() -> Self"),
        "Metal feature snapshot must retain the Metal-specific require constructor"
    );
    assert!(
        metal_snapshot.contains("pub wsi_rs::OutputBackendRequest::Metal"),
        "Metal feature snapshot must expose the Metal backend request variant"
    );
    assert!(
        !metal_snapshot.contains("pub wsi_rs::OutputBackendRequest::Cuda"),
        "Metal feature snapshot must not expose the experimental CUDA backend variant"
    );

    let require_metal = output
        .find("pub fn require_metal() -> Self")
        .expect("TileOutputPreference::require_metal must exist");
    let previous_function = output[..require_metal].rfind("pub fn ").unwrap_or_default();
    let metal_cfg = output[..require_metal]
        .rfind("#[cfg(feature = \"metal\")]")
        .expect("require_metal must be feature-gated");
    assert!(
        metal_cfg > previous_function,
        "require_metal must have its own #[cfg(feature = \"metal\")] gate"
    );

    for (variant, feature) in [("Metal", "metal"), ("Cuda", "cuda")] {
        let variant = output
            .find(&format!("{variant},"))
            .unwrap_or_else(|| panic!("OutputBackendRequest::{variant} must exist"));
        let previous_variant = output[..variant].rfind(',').unwrap_or_default();
        let feature_cfg = output[..variant]
            .rfind(&format!("#[cfg(feature = \"{feature}\")]"))
            .unwrap_or_else(|| panic!("OutputBackendRequest::{variant} must be feature-gated"));
        assert!(
            feature_cfg > previous_variant,
            "OutputBackendRequest::{variant} must have its own #[cfg(feature = \"{feature}\")] gate"
        );
    }
}

#[test]
fn public_api_extensible_enums_are_non_exhaustive() {
    for (relative, enum_name) in [
        ("src/error.rs", "WsiError"),
        ("src/core/registry/traits.rs", "ProbeConfidence"),
        ("src/core/decode_runtime.rs", "DecodeRoute"),
        ("src/core/types/geometry.rs", "TileLayout"),
        ("src/core/types/model.rs", "LevelSourceKind"),
        ("src/core/types/model.rs", "Compression"),
        ("src/core/types/model.rs", "TileCodecKind"),
        (
            "src/core/types/model.rs",
            "EncodedTilePhotometricInterpretation",
        ),
        ("src/core/types/output.rs", "OutputBackendRequest"),
        ("src/core/types/output.rs", "TileOutputPreference"),
        ("src/core/types/output.rs", "TilePixels"),
        ("src/core/types/output.rs", "DeviceTile"),
        ("src/core/types/pixels.rs", "SampleType"),
        ("src/core/types/pixels.rs", "PixelFormat"),
        ("src/core/types/pixels.rs", "CpuTileData"),
        ("src/core/types/pixels.rs", "ColorSpace"),
        ("src/core/types/pixels.rs", "CpuTileLayout"),
        ("src/formats/svcache.rs", "SvcachePolicy"),
        ("src/core/types/requests.rs", "RequestBuildError"),
    ] {
        assert_non_exhaustive_enum(relative, enum_name);
    }
}

#[test]
fn public_request_structs_are_non_exhaustive() {
    for (relative, struct_name) in [
        ("src/core/types/requests.rs", "RegionRequest"),
        ("src/core/types/requests.rs", "TileRequest"),
        ("src/core/types/requests.rs", "TileViewRequest"),
    ] {
        assert_non_exhaustive_struct(relative, struct_name);
    }
}

#[test]
fn public_tile_request_indices_use_stable_newtypes() {
    let requests = read_repo_text("src/core/types/requests.rs");
    let model = read_repo_text("src/core/types/model.rs");

    for required in [
        "pub scene: SceneId",
        "pub series: SeriesId",
        "pub level: LevelIdx",
        "pub plane: PlaneIdx",
        "pub fn new(\n        scene: impl Into<SceneId>,",
        "pub fn builder(\n        scene: impl Into<SceneId>,",
        "pub fn with_plane(mut self, plane: impl Into<PlaneIdx>) -> Self",
    ] {
        assert!(
            requests.contains(required),
            "tile and display tile requests must use stable public index newtypes; missing `{required}`"
        );
    }

    for forbidden in [
        "pub scene: usize",
        "pub series: usize",
        "pub level: u32",
        "pub plane: PlaneSelection",
        "pub fn new(scene: usize, series: usize, level: u32",
        "pub fn builder(scene: usize, series: usize, level: u32",
        "impl From<u8> for SceneId",
        "impl From<u8> for SeriesId",
        "impl From<u8> for LevelIdx",
    ] {
        assert!(
            !requests.contains(forbidden) && !model.contains(forbidden),
            "tile and display tile requests must not expose primitive index field/API `{forbidden}`"
        );
    }
}

#[test]
fn public_level_source_kind_uses_stable_newtypes() {
    let slide = read_repo_text("src/core/registry/slide.rs");
    let traits = read_repo_text("src/core/registry/traits.rs");

    for required in [
        "pub fn level_source_kind(",
        "scene: impl Into<SceneId>",
        "series: impl Into<SeriesId>",
        "level: impl Into<LevelIdx>",
        ".level_source_kind(scene.into(), series.into(), level.into())",
    ] {
        assert!(
            slide.contains(required),
            "Slide::level_source_kind must accept the same typed or plain public indices as read requests; missing `{required}`"
        );
    }

    for required in [
        "fn level_source_kind(",
        "scene: SceneId",
        "series: SeriesId",
        "level: LevelIdx",
    ] {
        assert!(
            traits.contains(required),
            "SlideReader::level_source_kind must keep concrete typed newtypes for object-safe backend implementations; missing `{required}`"
        );
    }

    for source in [&slide, &traits] {
        assert!(
            !source.contains("scene: usize,\n        series: usize,\n        level: u32"),
            "public level_source_kind APIs must not expose primitive scene/series/level indices"
        );
    }
}

#[test]
fn public_region_request_indices_match_tile_request_ergonomics() {
    let requests = read_repo_text("src/core/types/requests.rs");
    let region_impl = requests
        .split("impl RegionRequest {")
        .nth(1)
        .and_then(|tail| tail.split("/// Builder for [`RegionRequest`].").next())
        .expect("RegionRequest impl block must be present");

    for required in [
        "pub fn new(\n        scene: impl Into<SceneId>,",
        "series: impl Into<SeriesId>,",
        "level: impl Into<LevelIdx>,",
        "pub fn builder(\n        scene: impl Into<SceneId>,",
    ] {
        assert!(
            region_impl.contains(required),
            "RegionRequest constructors must accept the same typed or plain public indices as tile requests; missing `{required}`"
        );
    }
}

#[test]
fn public_request_plane_setters_accept_plain_plane_selection() {
    let requests = read_repo_text("src/core/types/requests.rs");
    let model = read_repo_text("src/core/types/model.rs");

    assert!(
        model.contains("impl From<PlaneSelection> for PlaneIdx"),
        "PlaneSelection must convert into PlaneIdx for ergonomic public request APIs"
    );

    for required in [
        "pub fn with_plane(mut self, plane: impl Into<PlaneIdx>) -> Self",
        "pub fn plane(mut self, plane: impl Into<PlaneIdx>) -> Self",
    ] {
        assert!(
            requests.contains(required),
            "request builders must accept PlaneSelection through `{required}`"
        );
    }
}

#[test]
fn public_index_newtypes_have_constructor_api_before_layout_freeze() {
    for (relative, struct_name) in [
        ("src/core/types/model.rs", "DatasetId"),
        ("src/core/types/model.rs", "SceneId"),
        ("src/core/types/model.rs", "SeriesId"),
        ("src/core/types/model.rs", "LevelIdx"),
        ("src/core/types/model.rs", "PlaneIdx"),
    ] {
        assert_non_exhaustive_struct(relative, struct_name);
    }

    let model = read_repo_text("src/core/types/model.rs");
    for required in [
        "pub struct DatasetId(pub(crate) u128)",
        "impl DatasetId",
        "pub const fn new(value: u128) -> Self",
        "pub const fn get(self) -> u128",
        "pub struct SceneId(pub(crate) usize)",
        "impl SceneId",
        "pub const fn new(index: usize) -> Self",
        "pub const fn get(self) -> usize",
        "pub struct SeriesId(pub(crate) usize)",
        "impl SeriesId",
        "pub struct LevelIdx(pub(crate) u32)",
        "impl LevelIdx",
        "pub const fn new(index: u32) -> Self",
        "pub const fn get(self) -> u32",
        "pub struct PlaneIdx(pub(crate) PlaneSelection)",
        "impl PlaneIdx",
        "pub const fn new(plane: PlaneSelection) -> Self",
        "pub const fn get(self) -> PlaneSelection",
    ] {
        assert!(
            model.contains(required),
            "public index newtypes must expose constructor/accessor API `{required}` before hiding tuple construction"
        );
    }
}

#[test]
fn public_probe_result_has_future_extensible_constructor_api() {
    assert_non_exhaustive_struct("src/core/registry/traits.rs", "ProbeResult");

    let traits = read_repo_text("src/core/registry/traits.rs");
    for required in [
        "impl ProbeResult",
        "pub fn detected(",
        "pub fn not_detected(",
    ] {
        assert!(
            traits.contains(required),
            "ProbeResult must expose `{required}` before hiding literal construction"
        );
    }
}

#[test]
fn public_configuration_and_diagnostic_structs_are_non_exhaustive() {
    for (relative, struct_name) in [
        ("src/core/cache.rs", "CacheConfig"),
        ("src/core/decode_runtime.rs", "DecodeExecutionOptions"),
        ("src/core/decode_runtime.rs", "DecodeRouteDecision"),
        ("src/core/registry/open_options.rs", "SlideOpenOptions"),
        ("src/core/types/output.rs", "DeviceOutputContext"),
        ("src/core/types/pixels.rs", "DisplayWindow"),
    ] {
        assert_non_exhaustive_struct(relative, struct_name);
    }
}

#[test]
fn public_display_window_has_constructor_api_before_non_exhaustive_freeze() {
    assert_non_exhaustive_struct("src/core/types/pixels.rs", "DisplayWindow");

    let pixels = read_repo_text("src/core/types/pixels.rs");
    assert!(
        pixels.contains("impl DisplayWindow")
            && pixels.contains("pub fn new(")
            && pixels.contains("window range must be positive"),
        "DisplayWindow must expose a validating constructor before hiding literal construction"
    );
    for forbidden in ["pub min: f64", "pub max: f64"] {
        assert!(
            !pixels.contains(forbidden),
            "DisplayWindow bounds must stay private so downstream code cannot bypass constructor validation with `{forbidden}`"
        );
    }

    for snapshot_path in [
        "api/wsi-rs-public-api.txt",
        "api/wsi-rs-public-api-cuda.txt",
        "api/wsi-rs-public-api-metal.txt",
    ] {
        let snapshot = fs::read_to_string(crate_root().join(snapshot_path))
            .unwrap_or_else(|err| panic!("read {snapshot_path}: {err}"));
        for required in [
            "pub fn wsi_rs::DisplayWindow::min(&self) -> f64",
            "pub fn wsi_rs::DisplayWindow::max(&self) -> f64",
        ] {
            assert!(
                snapshot.contains(required),
                "{snapshot_path} must expose read accessors for private DisplayWindow bounds; missing `{required}`"
            );
        }
        for forbidden in [
            "pub wsi_rs::DisplayWindow::min: f64",
            "pub wsi_rs::DisplayWindow::max: f64",
        ] {
            assert!(
                !snapshot.contains(forbidden),
                "{snapshot_path} must not expose public mutable DisplayWindow bounds; found `{forbidden}`"
            );
        }
    }
}

#[test]
fn public_metadata_and_pixel_structs_are_non_exhaustive() {
    for (relative, struct_name) in [
        ("src/core/types/model.rs", "Dataset"),
        ("src/core/types/model.rs", "IccProfileKey"),
        ("src/core/types/model.rs", "Scene"),
        ("src/core/types/model.rs", "Series"),
        ("src/core/types/model.rs", "AxesShape"),
        ("src/core/types/model.rs", "Level"),
        ("src/core/types/model.rs", "ChannelInfo"),
        ("src/core/types/model.rs", "AssociatedImage"),
        ("src/core/types/model.rs", "RawCompressedTile"),
        ("src/core/types/pixels.rs", "CpuTile"),
        ("src/properties.rs", "Properties"),
    ] {
        assert_non_exhaustive_struct(relative, struct_name);
    }
}

#[test]
fn public_cpu_tile_has_validated_read_only_api_before_non_exhaustive_freeze() {
    assert_non_exhaustive_struct("src/core/types/pixels.rs", "CpuTile");

    let pixels = read_repo_text("src/core/types/pixels.rs");
    for required in [
        "pub fn new(",
        "CpuTile invariant violated",
        "pub fn width(&self) -> u32",
        "pub fn height(&self) -> u32",
        "pub fn channels(&self) -> u16",
        "pub fn color_space(&self) -> &ColorSpace",
        "pub fn layout(&self) -> CpuTileLayout",
        "pub fn data(&self) -> &CpuTileData",
    ] {
        assert!(
            pixels.contains(required),
            "CpuTile must expose a validating constructor and read-only accessors before hiding fields; missing `{required}`"
        );
    }
    for forbidden in [
        "pub width: u32",
        "pub height: u32",
        "pub channels: u16",
        "pub color_space: ColorSpace",
        "pub layout: CpuTileLayout",
        "pub data: CpuTileData",
    ] {
        assert!(
            !pixels.contains(forbidden),
            "CpuTile fields must stay private to preserve constructor validation; found `{forbidden}`"
        );
    }

    for snapshot_path in [
        "api/wsi-rs-public-api.txt",
        "api/wsi-rs-public-api-cuda.txt",
        "api/wsi-rs-public-api-metal.txt",
    ] {
        let snapshot = fs::read_to_string(crate_root().join(snapshot_path))
            .unwrap_or_else(|err| panic!("read {snapshot_path}: {err}"));
        for required in [
            "pub fn wsi_rs::CpuTile::channels(&self) -> u16",
            "pub fn wsi_rs::CpuTile::color_space(&self) -> &wsi_rs::ColorSpace",
            "pub fn wsi_rs::CpuTile::data(&self) -> &wsi_rs::CpuTileData",
            "pub fn wsi_rs::CpuTile::layout(&self) -> wsi_rs::CpuTileLayout",
        ] {
            assert!(
                snapshot.contains(required),
                "{snapshot_path} must expose read-only CpuTile accessors; missing `{required}`"
            );
        }
        for forbidden in [
            "pub wsi_rs::CpuTile::channels:",
            "pub wsi_rs::CpuTile::color_space:",
            "pub wsi_rs::CpuTile::data:",
            "pub wsi_rs::CpuTile::height:",
            "pub wsi_rs::CpuTile::layout:",
            "pub wsi_rs::CpuTile::width:",
        ] {
            assert!(
                !snapshot.contains(forbidden),
                "{snapshot_path} must not expose mutable CpuTile fields; found `{forbidden}`"
            );
        }
    }
}

#[test]
fn public_raw_compressed_tile_has_validated_read_only_api_before_non_exhaustive_freeze() {
    assert_non_exhaustive_struct("src/core/types/model.rs", "RawCompressedTile");

    let model = read_repo_text("src/core/types/model.rs");
    for required in [
        "pub fn builder(compression: Compression) -> RawCompressedTileBuilder",
        "pub fn compression(&self) -> Compression",
        "pub fn width(&self) -> u32",
        "pub fn height(&self) -> u32",
        "pub fn bits_allocated(&self) -> u16",
        "pub fn samples_per_pixel(&self) -> u16",
        "pub fn photometric_interpretation(&self) -> EncodedTilePhotometricInterpretation",
        "pub fn data(&self) -> &[u8]",
        "pub fn into_data(self) -> Vec<u8>",
        "RawCompressedTileBuildError::InvalidDimensions",
        "RawCompressedTileBuildError::InvalidBitsAllocated",
        "RawCompressedTileBuildError::InvalidSamplesPerPixel",
        "RawCompressedTileBuildError::EmptyData",
    ] {
        assert!(
            model.contains(required),
            "RawCompressedTile must expose builder validation and read-only accessors before hiding fields; missing `{required}`"
        );
    }
    for forbidden in [
        "pub(crate) compression: Compression",
        "pub compression: Compression",
        "pub(crate) width: u32",
        "pub width: u32",
        "pub(crate) height: u32",
        "pub height: u32",
        "pub(crate) bits_allocated: u16",
        "pub bits_allocated: u16",
        "pub(crate) samples_per_pixel: u16",
        "pub samples_per_pixel: u16",
        "pub(crate) photometric_interpretation: EncodedTilePhotometricInterpretation",
        "pub photometric_interpretation: EncodedTilePhotometricInterpretation",
        "pub(crate) data: Vec<u8>",
        "pub data: Vec<u8>",
    ] {
        assert!(
            !model.contains(forbidden),
            "RawCompressedTile fields must stay private to preserve builder validation; found `{forbidden}`"
        );
    }

    for snapshot_path in [
        "api/wsi-rs-public-api.txt",
        "api/wsi-rs-public-api-cuda.txt",
        "api/wsi-rs-public-api-metal.txt",
    ] {
        let snapshot = fs::read_to_string(crate_root().join(snapshot_path))
            .unwrap_or_else(|err| panic!("read {snapshot_path}: {err}"));
        for required in [
            "pub fn wsi_rs::RawCompressedTile::bits_allocated(&self) -> u16",
            "pub fn wsi_rs::RawCompressedTile::compression(&self) -> wsi_rs::Compression",
            "pub fn wsi_rs::RawCompressedTile::data(&self) -> &[u8]",
            "pub fn wsi_rs::RawCompressedTile::height(&self) -> u32",
            "pub fn wsi_rs::RawCompressedTile::into_data(self) -> alloc::vec::Vec<u8>",
            "pub fn wsi_rs::RawCompressedTile::photometric_interpretation(&self) -> wsi_rs::EncodedTilePhotometricInterpretation",
            "pub fn wsi_rs::RawCompressedTile::samples_per_pixel(&self) -> u16",
            "pub fn wsi_rs::RawCompressedTile::width(&self) -> u32",
        ] {
            assert!(
                snapshot.contains(required),
                "{snapshot_path} must expose read-only RawCompressedTile accessors; missing `{required}`"
            );
        }
        for forbidden in [
            "pub wsi_rs::RawCompressedTile::bits_allocated:",
            "pub wsi_rs::RawCompressedTile::compression:",
            "pub wsi_rs::RawCompressedTile::data:",
            "pub wsi_rs::RawCompressedTile::height:",
            "pub wsi_rs::RawCompressedTile::photometric_interpretation:",
            "pub wsi_rs::RawCompressedTile::samples_per_pixel:",
            "pub wsi_rs::RawCompressedTile::width:",
        ] {
            assert!(
                !snapshot.contains(forbidden),
                "{snapshot_path} must not expose mutable RawCompressedTile fields; found `{forbidden}`"
            );
        }
    }
}

#[test]
fn raw_compressed_tile_construction_is_centralized_through_builder() {
    for relative in [
        "src/formats",
        "src/core/registry",
        "src/core/decode_runtime.rs",
        "src/bin",
        "examples",
    ] {
        let source = read_repo_text(relative);
        assert!(
            !source.contains("Ok(RawCompressedTile {"),
            "{relative} must construct raw compressed tile payloads through RawCompressedTile::builder so validation is centralized"
        );
    }
}

#[test]
fn public_metadata_structs_have_constructor_api_before_non_exhaustive_freeze() {
    let model = read_repo_text("src/core/types/model.rs");
    for required in [
        "impl Dataset",
        "pub fn new(id: DatasetId, scenes: Vec<Scene>) -> Self",
        "pub fn with_associated_images(",
        "pub fn with_properties(",
        "pub fn with_icc_profiles(",
        "impl IccProfileKey",
        "pub const fn new(scene: SceneId, series: SeriesId) -> Self",
        "impl Scene",
        "pub fn new(id: impl Into<String>, series: Vec<Series>) -> Self",
        "pub fn with_name(",
        "impl Series",
        "pub fn new(",
        "impl AxesShape",
        "pub const fn new(z: u32, c: u32, t: u32) -> Self",
        "impl Level",
        "pub fn new(dimensions: (u64, u64), downsample: f64, tile_layout: TileLayout) -> Self",
        "impl ChannelInfo",
        "pub fn new() -> Self",
        "pub fn with_color(",
        "impl AssociatedImage",
        "pub const fn new(dimensions: (u32, u32), sample_type: SampleType, channels: u16) -> Self",
        "impl RawCompressedTile",
        "pub fn builder(compression: Compression) -> RawCompressedTileBuilder",
        "pub struct RawCompressedTileBuilder",
        "pub enum RawCompressedTileBuildError",
    ] {
        assert!(
            model.contains(required),
            "metadata model must expose named construction API `{required}` before hiding literal construction"
        );
    }
}

#[test]
fn public_icc_profile_metadata_uses_stable_key_type() {
    let model = read_repo_text("src/core/types/model.rs");
    for required in [
        "pub struct IccProfileKey",
        "pub scene: SceneId",
        "pub series: SeriesId",
        "pub icc_profiles: HashMap<IccProfileKey, Vec<u8>>",
        "pub const fn new(scene: SceneId, series: SeriesId) -> Self",
        "pub fn with_icc_profiles(mut self, icc_profiles: HashMap<IccProfileKey, Vec<u8>>) -> Self",
    ] {
        assert!(
            model.contains(required),
            "ICC profile metadata must use a named typed key before the release-candidate API freeze; missing `{required}`"
        );
    }

    for forbidden in [
        "pub icc_profiles: HashMap<(usize, usize), Vec<u8>>",
        "with_icc_profiles(mut self, icc_profiles: HashMap<(usize, usize), Vec<u8>>)",
    ] {
        assert!(
            !model.contains(forbidden),
            "ICC profile metadata must not expose primitive tuple key `{forbidden}`"
        );
    }
}

#[test]
fn public_selection_and_geometry_structs_are_non_exhaustive() {
    for (relative, struct_name) in [
        ("src/core/types/requests.rs", "PlaneSelection"),
        ("src/core/types/geometry.rs", "TileEntry"),
        ("src/core/types/geometry.rs", "TileHit"),
        ("src/formats/svcache.rs", "SvcacheTileSelection"),
    ] {
        assert_non_exhaustive_struct(relative, struct_name);
    }
}

#[test]
fn public_selection_and_geometry_structs_have_constructor_api() {
    let requests = read_repo_text("src/core/types/requests.rs");
    assert!(
        requests.contains("impl PlaneSelection")
            && requests.contains("pub const fn new(z: u32, c: u32, t: u32) -> Self"),
        "PlaneSelection must expose a constructor before hiding literal construction"
    );

    let geometry = read_repo_text("src/core/types/geometry.rs");
    for required in [
        "impl TileEntry",
        "pub fn new(offset: (f64, f64), dimensions: (u32, u32)) -> Self",
        "pub fn with_tiff_tile_index(",
    ] {
        assert!(
            geometry.contains(required),
            "TileEntry must expose constructor API `{required}` before hiding literal construction"
        );
    }

    let svcache = read_repo_text("src/formats/svcache.rs");
    for required in [
        "impl SvcacheTileSelection",
        "pub fn new(",
        "scene: impl Into<SceneId>",
        "series: impl Into<SeriesId>",
        "level: impl Into<LevelIdx>",
        "pub fn with_plane(",
    ] {
        assert!(
            svcache.contains(required),
            "SvcacheTileSelection must expose constructor API `{required}` before hiding literal construction"
        );
    }
}

#[test]
fn public_svcache_tile_selection_uses_stable_newtypes() {
    let svcache = read_repo_text("src/formats/svcache.rs");
    for required in [
        "pub scene: SceneId",
        "pub series: SeriesId",
        "pub level: LevelIdx",
        "pub plane: PlaneIdx",
        "pub fn new(",
        "scene: impl Into<SceneId>",
        "series: impl Into<SeriesId>",
        "level: impl Into<LevelIdx>",
        "plane: impl Into<PlaneIdx>",
    ] {
        assert!(
            svcache.contains(required),
            "SvcacheTileSelection must use typed public indices before the release-candidate API freeze; missing `{required}`"
        );
    }

    for forbidden in [
        "pub scene: usize",
        "pub series: usize",
        "pub level: u32",
        "pub plane: PlaneSelection",
        "pub fn new(scene: usize, series: usize, level: u32",
    ] {
        assert!(
            !svcache.contains(forbidden),
            "SvcacheTileSelection must not expose primitive public indices `{forbidden}`"
        );
    }
}

#[test]
fn optional_metal_public_surface_is_future_extensible() {
    assert_non_exhaustive_struct("src/output/metal/tile.rs", "MetalDeviceTile");
    assert_non_exhaustive_enum("src/output/metal/tile.rs", "MetalDeviceStorage");
    assert_non_exhaustive_struct("src/output/cuda.rs", "CudaDeviceTile");
    assert_non_exhaustive_enum("src/output/cuda.rs", "CudaDeviceStorage");
}

#[test]
fn optional_cuda_public_surface_matches_device_tile_contract() {
    let cuda = read_repo_text("src/output/cuda.rs");
    for required in [
        "pub width: u32",
        "pub height: u32",
        "pub pitch_bytes: usize",
        "pub format: PixelFormat",
        "pub storage: CudaDeviceStorage",
        "j2k_jpeg_cuda::Surface",
        "j2k_cuda::Surface",
        "cuda_surface()",
    ] {
        assert!(
            cuda.contains(required),
            "CUDA device tile output must expose resident surface contract; missing `{required}`"
        );
    }

    let output = read_repo_text("src/core/types/output.rs");
    assert!(
        !output.contains("_phase5_placeholder"),
        "CudaDeviceTile must not remain a placeholder"
    );
}

#[test]
fn default_manifest_uses_cpu_jp2k_facade_and_optional_metal_adapter() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("read manifest");
    let manifest = manifest
        .parse::<toml::Value>()
        .expect("Cargo.toml must parse as TOML");

    let dependencies = manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .expect("Cargo.toml must define [dependencies]");
    assert!(
        dependencies.contains_key("j2k"),
        "wsi_rs default JP2K decode must depend on j2k facade"
    );

    let j2k_metal = dependencies
        .get("j2k-metal")
        .and_then(toml::Value::as_table)
        .expect("j2k-metal dependency must use table syntax");
    assert!(
        j2k_metal.get("optional").and_then(toml::Value::as_bool) == Some(true),
        "j2k-metal must be optional"
    );

    let features = manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .expect("Cargo.toml must define [features]");
    let metal_feature = features
        .get("metal")
        .and_then(toml::Value::as_array)
        .expect("metal feature must be an array");
    assert!(
        metal_feature
            .iter()
            .any(|value| value.as_str() == Some("dep:j2k-metal")),
        "metal feature must be the only feature that enables j2k-metal"
    );

    let enabling_features = features
        .iter()
        .filter_map(|(name, value)| {
            value.as_array().and_then(|items| {
                items
                    .iter()
                    .any(|item| item.as_str() == Some("dep:j2k-metal"))
                    .then_some(name.as_str())
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        enabling_features,
        vec!["metal"],
        "only the metal feature may enable j2k-metal"
    );
}
#[test]
fn metal_output_facade_delegates_resource_lifecycles_to_focused_modules() {
    let facade = read_repo_text("src/output/metal.rs");
    for required in [
        "mod interop;",
        "mod session;",
        "mod tile;",
        "mod ycbcr;",
        "pub use session::MetalBackendSessions;",
        "pub use tile::{MetalDeviceStorage, MetalDeviceTile};",
    ] {
        assert!(
            facade.contains(required),
            "Metal facade is missing `{required}`"
        );
    }
    for forbidden in [
        "pub struct MetalBackendSessions",
        "pub struct MetalDeviceTile",
        "pub enum MetalDeviceStorage",
        "struct YcbcrAddressPlan",
    ] {
        assert!(
            !facade.contains(forbidden),
            "Metal facade owns `{forbidden}`"
        );
    }
    for relative in [
        "src/output/metal/session.rs",
        "src/output/metal/tile.rs",
        "src/output/metal/ycbcr.rs",
        "src/output/metal/interop.rs",
        "src/output/metal/ycbcr.metal",
    ] {
        assert!(crate_root().join(relative).is_file(), "missing {relative}");
    }
}
