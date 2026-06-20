use super::*;
use std::sync::atomic::Ordering;

fn mirax_sentinel_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../downloads/openslide-testdata-extracted/mirax/mirax-cmu1/CMU-1.mrxs")
}

#[test]
fn associated_thumbnail_is_cached_after_first_read() {
    let sentinel_path = mirax_sentinel_path();
    if !sentinel_path.is_file() {
        eprintln!(
            "skipping corpus-backed MIRAX thumbnail cache test; missing {}",
            sentinel_path.display()
        );
        return;
    }
    MIRAX_ASSOCIATED_CACHE_HITS.store(0, Ordering::Relaxed);
    let slide = MiraxSlide::parse(&sentinel_path).expect("parse MIRAX sentinel");
    let first = slide
        .read_associated("thumbnail")
        .expect("read thumbnail once");
    let second = slide
        .read_associated("thumbnail")
        .expect("read thumbnail twice");
    assert_eq!(first.width, second.width);
    assert_eq!(first.height, second.height);
    assert_eq!(
        MIRAX_ASSOCIATED_CACHE_HITS.load(Ordering::Relaxed),
        1,
        "second thumbnail read should hit the cache"
    );
}
