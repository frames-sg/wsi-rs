use super::*;
use std::io::Write;
use std::sync::atomic::Ordering;

mod backend;
mod errors;
pub(super) mod fixtures;
mod parser;

static MIRAX_ASSOCIATED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn mirax_sentinel_path() -> PathBuf {
    let cache = std::env::var_os("WSI_RS_PARITY_CORPUS_CACHE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join(".cache")
                    .join("slideviewer")
                    .join("parity-corpus")
            })
        });
    cache
        .map(|cache| cache.join("mirax-001.d").join("CMU-1.mrxs"))
        .unwrap_or_else(|| PathBuf::from("mirax-001.d/CMU-1.mrxs"))
}

#[test]
fn associated_thumbnail_is_cached_after_first_read() {
    let _serial = MIRAX_ASSOCIATED_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
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

#[test]
fn truncated_quickhash_range_returns_contextual_unexpected_eof_without_prefix_hash() {
    let mut source = tempfile::NamedTempFile::new().expect("temporary MIRAX data file");
    source.write_all(b"abcd").expect("write MIRAX data");
    source.flush().expect("flush MIRAX data");
    let mut files = HashMap::new();
    let mut quickhash = Quickhash1::new();

    let error =
        helpers::quickhash_file_part_cached(&mut quickhash, &mut files, source.path(), 2, 4)
            .expect_err("declared MIRAX range past EOF must not produce a prefix hash");

    let WsiError::IoWithPath { source: io, path } = error else {
        panic!("expected contextual I/O error, got {error:?}");
    };
    assert_eq!(io.kind(), std::io::ErrorKind::UnexpectedEof);
    assert_eq!(path, source.path());
    assert!(io.to_string().contains("offset 2"), "{io}");
    assert!(io.to_string().contains("4 bytes"), "{io}");
    assert_eq!(
        quickhash.finish(),
        Quickhash1::new().finish(),
        "failed range must not commit a prefix into the dataset hash"
    );
}
