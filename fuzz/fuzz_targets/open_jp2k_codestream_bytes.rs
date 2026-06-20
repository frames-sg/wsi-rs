#![no_main]

use libfuzzer_sys::fuzz_target;
use wsi_rs::{Slide, TileOutputPreference, TileRequest};

const MAX_INPUT_BYTES: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let Ok(dir) = tempfile::Builder::new().prefix("wsi_rs-fuzz-").tempdir() else {
        return;
    };
    let path = dir.path().join("input.j2k");
    if std::fs::write(&path, data).is_err() {
        return;
    }

    let Ok(slide) = Slide::open(&path) else {
        return;
    };
    let request = TileRequest::new(0, 0, 0, 0, 0);
    let _ = slide.read_tile(&request, TileOutputPreference::cpu());
});
