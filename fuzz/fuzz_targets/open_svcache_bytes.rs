#![no_main]

use libfuzzer_sys::fuzz_target;
use statumen::Slide;

const MAX_INPUT_BYTES: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let Ok(dir) = tempfile::Builder::new().prefix("statumen-fuzz-").tempdir() else {
        return;
    };
    let path = dir.path().join("input.svcache");
    if std::fs::write(&path, data).is_err() {
        return;
    }

    let Ok(slide) = Slide::open(&path) else {
        return;
    };
    let _ = slide.dataset();
});
