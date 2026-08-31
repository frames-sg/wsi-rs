#![no_main]

use libfuzzer_sys::fuzz_target;
use wsi_rs::Slide;

const MAX_INPUT_BYTES: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let Ok(dir) = tempfile::Builder::new().prefix("wsi-rs-vms-fuzz-").tempdir() else {
        return;
    };
    let key = dir.path().join("input.vms");
    for name in ["image0.jpg", "image1.jpg", "map.jpg", "macro.jpg", "optimisation.bin"] {
        if std::fs::write(dir.path().join(name), data).is_err() {
            return;
        }
    }
    if std::fs::write(&key, data).is_ok() {
        let _ = Slide::open(key);
    }
});
