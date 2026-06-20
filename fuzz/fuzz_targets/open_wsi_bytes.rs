#![no_main]

use libfuzzer_sys::fuzz_target;
use wsi_rs::{LevelIdx, RegionRequest, SceneId, SeriesId, Slide};

const MAX_INPUT_BYTES: usize = 1 << 20;
const MAX_REGION_SIDE: u64 = 32;
const WSI_FUZZ_EXTENSIONS: &[&str] = &[
    "svs", "ndpi", "scn", "tif", "tiff", "bif", "mrxs", "vms", "vmu", "vsi", "dcm", "czi", "zvi",
];

fuzz_target!(|data: &[u8]| {
    let Some((&selector, payload)) = data.split_first() else {
        return;
    };
    let extension = WSI_FUZZ_EXTENSIONS[usize::from(selector) % WSI_FUZZ_EXTENSIONS.len()];
    exercise_open(payload, extension);
});

fn exercise_open(data: &[u8], extension: &str) {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let Ok(dir) = tempfile::Builder::new().prefix("wsi_rs-fuzz-").tempdir() else {
        return;
    };
    let path = dir.path().join(format!("input.{extension}"));
    if std::fs::write(&path, data).is_err() {
        return;
    }

    let Ok(slide) = Slide::open(&path) else {
        return;
    };
    let Some(level) = slide
        .dataset()
        .scenes
        .first()
        .and_then(|scene| scene.series.first())
        .and_then(|series| series.levels.first())
    else {
        return;
    };

    let width = level.dimensions.0.min(MAX_REGION_SIDE) as u32;
    let height = level.dimensions.1.min(MAX_REGION_SIDE) as u32;
    if width == 0 || height == 0 {
        return;
    }

    let request = RegionRequest::new(
        SceneId::new(0),
        SeriesId::new(0),
        LevelIdx::new(0),
        (0, 0),
        (width, height),
    );
    let _ = slide.read_region_rgba(&request);
}
