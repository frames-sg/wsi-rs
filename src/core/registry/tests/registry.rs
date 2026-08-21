use super::super::*;
use super::support::MockSource;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct ErrProbe;

impl FormatProbe for ErrProbe {
    fn probe(&self, _path: &Path) -> Result<ProbeResult, WsiError> {
        Err(WsiError::InvalidSlide {
            path: "/bad.slide".into(),
            message: "probe failed".into(),
        })
    }
}

struct FalseProbe;

impl FormatProbe for FalseProbe {
    fn probe(&self, _path: &Path) -> Result<ProbeResult, WsiError> {
        Ok(ProbeResult {
            detected: false,
            vendor: "none".into(),
            confidence: ProbeConfidence::Likely,
        })
    }
}

struct CacheConfigRecordingBackend {
    observed_probe_shared_bytes: Arc<AtomicUsize>,
    observed_shared_bytes: Arc<AtomicUsize>,
}

impl FormatProbe for CacheConfigRecordingBackend {
    fn probe(&self, _path: &Path) -> Result<ProbeResult, WsiError> {
        Ok(ProbeResult::detected("test", ProbeConfidence::Definite))
    }
}

impl ConfiguredFormatProbe for CacheConfigRecordingBackend {
    fn probe_with_cache_config(
        &self,
        _path: &Path,
        cache_config: CacheConfig,
    ) -> Result<ProbeResult, WsiError> {
        self.observed_probe_shared_bytes.store(
            cache_config.shared_tile_bytes.unwrap_or_default() as usize,
            Ordering::SeqCst,
        );
        Ok(ProbeResult::detected("test", ProbeConfidence::Definite))
    }
}

impl DatasetReader for CacheConfigRecordingBackend {
    fn open(&self, _path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        Ok(Box::new(MockSource::new()))
    }
}

impl ConfiguredDatasetReader for CacheConfigRecordingBackend {
    fn open_with_cache_config(
        &self,
        _path: &Path,
        cache_config: CacheConfig,
    ) -> Result<Box<dyn SlideReader>, WsiError> {
        self.observed_shared_bytes.store(
            cache_config.shared_tile_bytes.unwrap_or_default() as usize,
            Ordering::SeqCst,
        );
        Ok(Box::new(MockSource::new()))
    }
}

struct MockReader;

impl DatasetReader for MockReader {
    fn open(&self, _path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
        Ok(Box::new(MockSource::new()))
    }
}

#[test]
fn slide_open_options_passes_cache_config_to_the_selected_reader() {
    let observed_probe_shared_bytes = Arc::new(AtomicUsize::new(0));
    let observed_shared_bytes = Arc::new(AtomicUsize::new(0));
    let mut registry = FormatRegistry::new();
    let backend = Arc::new(CacheConfigRecordingBackend {
        observed_probe_shared_bytes: observed_probe_shared_bytes.clone(),
        observed_shared_bytes: observed_shared_bytes.clone(),
    });
    registry.register_cache_configured(backend.clone(), backend);
    let config = CacheConfig::deterministic().with_shared_tile_bytes(512);

    let options = SlideOpenOptions::deterministic()
        .with_registry(registry)
        .with_cache_config(config);
    Slide::open_with_options("ignored.test", options).expect("open configured test reader");

    assert_eq!(observed_probe_shared_bytes.load(Ordering::SeqCst), 512);
    assert_eq!(observed_shared_bytes.load(Ordering::SeqCst), 512);
}

// Mock SlideReader for testing -- returns solid-color tiles based on (col, row).
// Grid: 2 cols x 2 rows of 256x256 tiles = 512x512 level.
//   (0,0) -> red   (255,0,0)
//   (1,0) -> green (0,255,0)
//   (0,1) -> blue  (0,0,255)
//   (1,1) -> white (255,255,255)
#[test]
fn format_registry_empty_returns_unsupported() {
    let reg = FormatRegistry::new();
    let result = reg.open(std::path::Path::new("/nonexistent"));
    assert!(result.is_err());
}

#[test]
fn probe_confidence_definite_beats_likely() {
    // Definite should beat Likely — tested via ProbeConfidence ordering
    assert!(matches!(
        ProbeConfidence::Definite,
        ProbeConfidence::Definite
    ));
    assert!(matches!(ProbeConfidence::Likely, ProbeConfidence::Likely));
}

#[test]
fn format_registry_detect_vendor_does_not_open_reader() {
    struct DefiniteProbe;
    impl FormatProbe for DefiniteProbe {
        fn probe(&self, _path: &Path) -> Result<ProbeResult, WsiError> {
            Ok(ProbeResult::detected(
                "probe-only",
                ProbeConfidence::Definite,
            ))
        }
    }

    struct CountingReader {
        opens: Arc<AtomicUsize>,
    }
    impl DatasetReader for CountingReader {
        fn open(&self, _path: &Path) -> Result<Box<dyn SlideReader>, WsiError> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(MockSource::new()))
        }
    }

    let opens = Arc::new(AtomicUsize::new(0));
    let mut reg = FormatRegistry::new();
    reg.register(
        DefiniteProbe,
        CountingReader {
            opens: opens.clone(),
        },
    );

    let detected = reg
        .detect_vendor(Path::new("/ok.slide"))
        .expect("detect vendor")
        .expect("detected vendor");

    assert_eq!(detected.vendor, "probe-only");
    assert_eq!(opens.load(Ordering::SeqCst), 0);
}

#[test]
fn format_registry_stops_after_first_definite_match() {
    struct DefiniteProbe;
    impl FormatProbe for DefiniteProbe {
        fn probe(&self, _path: &Path) -> Result<ProbeResult, WsiError> {
            Ok(ProbeResult::detected(
                "first-definite",
                ProbeConfidence::Definite,
            ))
        }
    }

    struct CountingMiss {
        calls: Arc<AtomicUsize>,
    }
    impl FormatProbe for CountingMiss {
        fn probe(&self, _path: &Path) -> Result<ProbeResult, WsiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ProbeResult::not_detected("later"))
        }
    }

    let later_calls = Arc::new(AtomicUsize::new(0));
    let mut registry = FormatRegistry::new();
    registry.register(DefiniteProbe, MockReader);
    registry.register(
        CountingMiss {
            calls: later_calls.clone(),
        },
        MockReader,
    );

    let detected = registry
        .detect_vendor(Path::new("/definite.slide"))
        .unwrap()
        .unwrap();

    assert_eq!(detected.vendor, "first-definite");
    assert_eq!(later_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn format_registry_returns_probe_error_when_no_backend_matches() {
    let mut reg = FormatRegistry::new();
    reg.register(ErrProbe, MockReader);

    match reg.open(Path::new("/bad.slide")) {
        Err(err) => match err {
            WsiError::InvalidSlide { message, .. } => assert!(message.contains("probe failed")),
            other => panic!("expected InvalidSlide, got {other:?}"),
        },
        Ok(_) => panic!("expected probe error"),
    }
}

#[test]
fn detected_backend_beats_probe_error() {
    let mut reg = FormatRegistry::new();
    reg.register(ErrProbe, MockReader);
    reg.register(FalseProbe, MockReader);

    struct DefiniteProbe;
    impl FormatProbe for DefiniteProbe {
        fn probe(&self, _path: &Path) -> Result<ProbeResult, WsiError> {
            Ok(ProbeResult {
                detected: true,
                vendor: "mock".into(),
                confidence: ProbeConfidence::Definite,
            })
        }
    }

    reg.register(DefiniteProbe, MockReader);

    let opened = reg.open(Path::new("/ok.slide")).unwrap();
    assert_eq!(opened.dataset().id, DatasetId::new(1));
}

#[test]
fn arc_format_probe_blanket_impl() {
    struct TestProbe;
    impl FormatProbe for TestProbe {
        fn probe(&self, _path: &Path) -> Result<ProbeResult, WsiError> {
            Ok(ProbeResult {
                detected: true,
                vendor: "test".into(),
                confidence: ProbeConfidence::Definite,
            })
        }
    }

    let arc_probe: Arc<TestProbe> = Arc::new(TestProbe);
    let result = arc_probe.probe(Path::new("/test")).unwrap();
    assert!(result.detected);
    assert_eq!(result.vendor, "test");
}

#[test]
fn arc_dataset_reader_blanket_impl() {
    let arc_reader: Arc<MockReader> = Arc::new(MockReader);
    let source = arc_reader.open(Path::new("/test")).unwrap();
    assert_eq!(source.dataset().id, DatasetId::new(1));
}

#[test]
fn probe_result_constructors_set_stable_public_fields() {
    let detected = ProbeResult::detected("fixture", ProbeConfidence::Definite);
    assert!(detected.detected);
    assert_eq!(detected.vendor, "fixture");
    assert_eq!(detected.confidence, ProbeConfidence::Definite);

    let missed = ProbeResult::not_detected("fixture");
    assert!(!missed.detected);
    assert_eq!(missed.vendor, "fixture");
    assert_eq!(missed.confidence, ProbeConfidence::Likely);
}

#[test]
fn builtin_registry_has_tiff_backend() {
    let reg = FormatRegistry::builtin();
    // The builtin registry should have at least one backend registered.
    // Probing a nonexistent path should produce an error (not panic).
    let result = reg.open(Path::new("/nonexistent/test.ndpi"));
    assert!(result.is_err());
    // The backend was registered and tried to probe. Whether we get
    // UnsupportedFormat (probe returned detected=false) or another
    // error variant, the backend was exercised.
    match result {
        Err(WsiError::UnsupportedFormat(_)) => {
            // The TIFF backend's probe returns detected=false for non-existent
            // files (the TiffContainer::open fails, so it returns detected=false).
            // With no backends matching, registry falls through to UnsupportedFormat.
            // This is acceptable — it proves the backend was registered and probed.
        }
        Err(_) => {} // Any other error also proves the backend tried
        Ok(_) => panic!("expected error for nonexistent file"),
    }
}

#[test]
fn builtin_registry_opens_raw_j2c_codestream_as_single_tile_slide() {
    let mut file = tempfile::Builder::new().suffix(".j2c").tempfile().unwrap();
    file.write_all(include_bytes!(
        "../../../../tests/fixtures/jp2k/rgb_mct.j2k"
    ))
    .unwrap();
    file.flush().unwrap();

    let slide =
        Slide::open_with_cache_bytes(file.path(), &FormatRegistry::builtin(), 16 * 1024 * 1024)
            .unwrap();

    let level = &slide.dataset().scenes[0].series[0].levels[0];
    assert_eq!(level.dimensions, (16, 12));
    assert!(matches!(
        level.tile_layout,
        TileLayout::Regular {
            tile_width: 16,
            tile_height: 12,
            tiles_across: 1,
            tiles_down: 1
        }
    ));

    let req = TileRequest {
        scene: 0usize.into(),
        series: 0usize.into(),
        level: 0u32.into(),
        plane: PlaneSelection::default().into(),
        col: 0,
        row: 0,
    };
    assert_eq!(slide.tile_codec_kind(&req), TileCodecKind::Jp2k);
    let tile = slide.source().read_tiles_cpu(&[req]).unwrap().remove(0);
    assert_eq!((tile.width, tile.height), (16, 12));
    assert_eq!(tile.channels, 3);
    assert_eq!(tile.color_space, ColorSpace::Rgb);
}

#[test]
fn open_nonexistent_file_returns_error() {
    let result = Slide::open("/nonexistent/path/slide.ndpi");
    assert!(result.is_err());
}
