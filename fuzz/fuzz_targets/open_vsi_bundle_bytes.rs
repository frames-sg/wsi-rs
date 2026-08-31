#![no_main]

use libfuzzer_sys::fuzz_target;
use wsi_rs::Slide;

const MAX_INPUT_BYTES: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let Ok(dir) = tempfile::Builder::new().prefix("wsi-rs-vsi-fuzz-").tempdir() else {
        return;
    };
    let entry = dir.path().join("input.vsi");
    let scene = dir.path().join("_input_").join("scene-0");
    if std::fs::create_dir_all(&scene).is_err()
        || std::fs::write(&entry, data).is_err()
        || std::fs::write(scene.join("frame_t.ets"), data).is_err()
    {
        return;
    }
    let _ = Slide::open(entry);
});
