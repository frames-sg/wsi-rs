use super::*;
use std::io::Write;
use std::sync::atomic::Ordering;

mod backend;
mod errors;
pub(super) mod fixtures;
mod parser;

static MIRAX_ASSOCIATED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(unix)]
#[test]
fn record_read_does_not_move_another_readers_shared_file_position() {
    let fixture = fixtures::MiraxFixture::complete();
    let mut file = File::open(&fixture.data_path).unwrap();
    let mut other = file.try_clone().unwrap();
    let expected = std::fs::read(&fixture.data_path).unwrap();
    let ready = std::sync::Barrier::new(2);
    let finished = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            other.seek(SeekFrom::Start(0)).unwrap();
            ready.wait();
            finished.wait();
            let mut actual = [0; 16];
            other.read_exact(&mut actual).unwrap();
            actual
        });
        ready.wait();
        let actual = helpers::read_record_bytes_from_file_with_limit(
            &mut file,
            &fixture.data_path,
            128,
            16,
            1024,
        )
        .unwrap();
        finished.wait();
        assert_eq!(actual, expected[128..144]);
        assert_eq!(reader.join().unwrap(), expected[..16]);
    });
}

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
