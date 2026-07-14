#![no_main]

use libfuzzer_sys::fuzz_target;
use wsi_rs::Slide;

const MAX_INPUT_BYTES: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let Ok(dir) = tempfile::Builder::new().prefix("wsi-rs-zvi-fuzz-").tempdir() else {
        return;
    };
    let path = dir.path().join("input.zvi");
    if std::fs::write(&path, data).is_ok() {
        let _ = Slide::open(path);
    }
});
