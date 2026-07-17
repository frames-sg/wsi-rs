#![cfg(all(feature = "metal", target_os = "macos"))]

use wsi_rs::{
    output::metal::MetalBackendSessions, DeviceTile, Slide, TileLayout, TileOutputPreference,
    TilePixels, TileRequest,
};

#[test]
#[ignore = "requires WSI_RS_METAL_EDGE_PATHS with local WSI fixtures"]
fn configured_wsi_edges_have_identical_cpu_and_metal_dimensions() {
    let raw_paths = std::env::var_os("WSI_RS_METAL_EDGE_PATHS")
        .expect("WSI_RS_METAL_EDGE_PATHS is required for real Metal edge conformance");
    let paths = std::env::split_paths(&raw_paths).collect::<Vec<_>>();
    assert!(!paths.is_empty(), "at least one WSI fixture is required");
    let device = metal::Device::system_default().expect("a Metal device is required");
    let sessions = MetalBackendSessions::new(device);

    for path in paths {
        let slide = Slide::open(&path).expect("open WSI fixture");
        let levels = &slide.dataset().scenes[0].series[0].levels;
        for (level_index, level) in levels.iter().enumerate() {
            let TileLayout::Regular {
                tiles_across,
                tiles_down,
                ..
            } = level.tile_layout
            else {
                continue;
            };
            let request = TileRequest::new(
                0usize,
                0usize,
                level_index as u32,
                i64::try_from(tiles_across.saturating_sub(1)).expect("tile column fits i64"),
                i64::try_from(tiles_down.saturating_sub(1)).expect("tile row fits i64"),
            );
            let raw = slide
                .source()
                .read_raw_compressed_tile(&request)
                .expect("read raw compressed edge tile");
            let TilePixels::Cpu(cpu) = slide
                .read_tile(&request, TileOutputPreference::cpu())
                .expect("read CPU edge tile")
            else {
                panic!("CPU edge request returned a device tile");
            };
            let device_tile = slide
                .read_tile(
                    &request,
                    TileOutputPreference::require_device_auto_with_metal_and_compressed_decode(
                        sessions.clone(),
                    ),
                )
                .expect("read Metal edge tile");
            let TilePixels::Device(DeviceTile::Metal(metal)) = device_tile else {
                panic!("Metal edge request did not return a Metal tile");
            };
            assert_eq!(
                (metal.width, metal.height),
                (cpu.width(), cpu.height()),
                "edge dimensions differ for {} level {level_index}; raw={}x{}",
                path.display(),
                raw.width(),
                raw.height(),
            );
            eprintln!(
                "{} level {level_index}: {}x{}",
                path.display(),
                metal.width,
                metal.height
            );
        }
    }
}
