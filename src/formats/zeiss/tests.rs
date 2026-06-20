use super::slide::*;
use super::*;

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static ZEISS_TEST_GUARD: Mutex<()> = Mutex::new(());

fn zeiss_uncompressed_fixture() -> Option<PathBuf> {
    if let Some(path) = env::var_os("WSI_RS_ZEISS_CZI_PATH").map(PathBuf::from) {
        return path.is_file().then_some(path);
    }

    let local = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("SlideViewer")
        .join("downloads")
        .join("openslide-testdata")
        .join("Zeiss")
        .join("Zeiss-5-Uncompressed.czi");
    local.is_file().then_some(local)
}

fn zeiss_fixture_or_skip() -> Option<PathBuf> {
    let path = zeiss_uncompressed_fixture();
    if path.is_none() {
        eprintln!("[zeiss] skipping: set WSI_RS_ZEISS_CZI_PATH to Zeiss-5-Uncompressed.czi");
    }
    path
}

#[test]
fn uncompressed_sentinel_hits_local_tile_path() {
    let _guard = ZEISS_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    ZEISS_LOCAL_TILE_HITS.store(0, Ordering::Relaxed);
    ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS.store(0, Ordering::Relaxed);
    let Some(path) = zeiss_fixture_or_skip() else {
        return;
    };
    let handle = crate::core::registry::Slide::open(&path).expect("open Zeiss sentinel");
    assert_eq!(handle.dataset().scenes.len(), 1);
    assert_eq!(handle.dataset().scenes[0].series.len(), 1);
    assert_eq!(handle.dataset().scenes[0].series[0].levels.len(), 5);
    assert_eq!(
        handle.dataset().scenes[0].series[0].levels[0].dimensions,
        (50171, 11340)
    );
    assert_eq!(
        handle.dataset().properties.get("openslide.region[0].x"),
        Some("0")
    );
    assert_eq!(
        handle.dataset().properties.get("openslide.region[0].y"),
        Some("2")
    );
    assert_eq!(
        handle.dataset().properties.get("openslide.region[1].x"),
        Some("38866")
    );
    assert_eq!(
        handle.dataset().properties.get("openslide.region[1].y"),
        Some("0")
    );
    let req = crate::core::types::TileViewRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: crate::core::types::PlaneSelection::default().into(),
        col: 0,
        row: 0,
        tile_width: 256,
        tile_height: 256,
    };
    let _ = handle
        .read_display_tile(&req)
        .expect("read Zeiss display tile");
    assert!(ZEISS_LOCAL_TILE_HITS.load(Ordering::Relaxed) > 0);
    assert!(ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS.load(Ordering::Relaxed) > 0);
}

#[test]
fn uncompressed_sentinel_pan_trace_l0_reads_successfully() {
    let _guard = ZEISS_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let Some(path) = zeiss_fixture_or_skip() else {
        return;
    };
    let handle = crate::core::registry::Slide::open(&path).expect("open Zeiss sentinel");
    let dims = handle.dataset().scenes[0].series[0].levels[0].dimensions;
    let tile_px = 256i64;
    let center = ((dims.0 / 2) as i64, (dims.1 / 2) as i64);
    let coords: Vec<(i64, i64)> = (0..256)
        .map(|i| {
            let delta = (i as i64 - 128) * tile_px;
            (center.0 + delta, center.1 + delta)
        })
        .filter(|&(x, y)| {
            x >= 0 && y >= 0 && x + tile_px <= dims.0 as i64 && y + tile_px <= dims.1 as i64
        })
        .collect();

    assert!(!coords.is_empty(), "expected pan_trace_l0 coordinates");
    for &(x, y) in &coords {
        let req = crate::core::types::TileViewRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: crate::core::types::PlaneSelection::default().into(),
            col: x.div_euclid(tile_px),
            row: y.div_euclid(tile_px),
            tile_width: tile_px as u32,
            tile_height: tile_px as u32,
        };
        let _ = handle
            .read_display_tile(&req)
            .expect("read Zeiss pan trace tile");
    }
}

#[test]
fn uncompressed_sentinel_gap_tile_is_blank() {
    let _guard = ZEISS_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let Some(path) = zeiss_fixture_or_skip() else {
        return;
    };
    let handle = crate::core::registry::Slide::open(&path).expect("open Zeiss sentinel");
    let req = crate::core::types::TileViewRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: crate::core::types::PlaneSelection::default().into(),
        col: 37,
        row: 37,
        tile_width: 256,
        tile_height: 256,
    };
    let tile = handle.read_display_tile(&req).expect("read Zeiss gap tile");
    assert!(
        tile.data.as_u8().unwrap().iter().all(|&byte| byte == 0),
        "expected the no-intersection tile to be blank"
    );
}

#[test]
fn uncompressed_sentinel_top_left_tile_is_not_blank() {
    let _guard = ZEISS_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS.store(0, Ordering::Relaxed);
    let Some(path) = zeiss_fixture_or_skip() else {
        return;
    };
    let slide = ZeissSlide::parse(&path).expect("parse Zeiss sentinel");
    let candidate_indices = slide.canvas_level_subblocks[0].clone();
    assert!(
        !candidate_indices.is_empty(),
        "expected level-0 Zeiss subblocks on the shared canvas"
    );
    let handle = crate::core::registry::Slide::open(&path).expect("open Zeiss sentinel");
    let req = crate::core::types::TileViewRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: crate::core::types::PlaneSelection::default().into(),
        col: 0,
        row: 0,
        tile_width: 256,
        tile_height: 256,
    };
    let tile = handle
        .read_display_tile(&req)
        .expect("read Zeiss top-left tile");
    assert!(
        tile.data.as_u8().unwrap().iter().any(|&byte| byte != 0),
        "expected the top-left tile on the shared Zeiss canvas to contain visible pixels"
    );
    assert!(ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS.load(Ordering::Relaxed) > 0);
}

#[test]
fn uncompressed_sentinel_levels_use_direct_composition() {
    let _guard = ZEISS_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    ZEISS_DIRECT_LEVEL_COMPOSE_HITS.store(0, Ordering::Relaxed);
    ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS.store(0, Ordering::Relaxed);
    let Some(path) = zeiss_fixture_or_skip() else {
        return;
    };
    let slide = ZeissSlide::parse(&path).expect("parse Zeiss sentinel");
    let scene = 0;
    let level = slide.dataset.scenes[scene].series[0].levels.len() - 1;
    let image = slide
        .scene_level_image(scene, level)
        .expect("compose Zeiss level from subblocks");
    let expected = slide.dataset.scenes[scene].series[0].levels[level].dimensions;

    assert_eq!(image.width, expected.0 as u32);
    assert_eq!(image.height, expected.1 as u32);
    assert_eq!(ZEISS_DIRECT_LEVEL_COMPOSE_HITS.load(Ordering::Relaxed), 1);
    assert!(ZEISS_DIRECT_UNCOMPRESSED_BLIT_HITS.load(Ordering::Relaxed) > 0);
}
