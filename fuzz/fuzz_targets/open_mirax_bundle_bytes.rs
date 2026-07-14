#![no_main]

use libfuzzer_sys::fuzz_target;
use wsi_rs::Slide;

const MAX_INPUT_BYTES: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let Ok(dir) = tempfile::Builder::new().prefix("wsi-rs-mirax-fuzz-").tempdir() else {
        return;
    };
    let entry = dir.path().join("input.mrxs");
    let bundle = dir.path().join("input");
    if std::fs::create_dir(&bundle).is_err()
        || std::fs::write(&entry, b"").is_err()
        || std::fs::write(bundle.join("Slidedat.ini"), data).is_err()
        || std::fs::write(bundle.join("Index.dat"), data).is_err()
        || std::fs::write(bundle.join("Data0.dat"), data).is_err()
    {
        return;
    }
    let _ = Slide::open(entry);
});
