#![no_main]

use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    if let Ok(xml) = std::str::from_utf8(data) {
        let _ = wsi_rs::fuzz_parse_xml(xml);
    }
});
