use super::fixtures::*;
use super::*;
#[test]
fn dicom_parse_keeps_encapsulated_frame_index_lazy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("htj2k-rpcl.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );

    let slide = DicomSlide::parse(&path).expect("parse DICOM slide");

    let image = &slide.levels[0].parts[0];
    assert!(
        image
            .frame_store
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none(),
        "encapsulated frame index should stay lazy until first frame read"
    );
}

#[test]
fn prepare_level_controlled_builds_the_lazy_frame_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prepare-level.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let slide = Arc::new(DicomSlide::parse(&path).expect("parse DICOM slide"));
    let image = slide.levels[0].parts[0].clone();
    let reader = DicomReader { slide };
    let handle = Slide::from_source_with_cache_bytes(Box::new(reader), 1024 * 1024);

    handle
        .prepare_level_controlled(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(0),
            &crate::ReadControl::default(),
        )
        .expect("prepare DICOM level");

    assert!(
        image
            .frame_store
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_some(),
        "preparation should publish the complete frame index"
    );
}

#[test]
fn controlled_preparation_reports_fast_index_build_then_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prepare-level-diagnostics.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let slide = Arc::new(DicomSlide::parse(&path).expect("parse DICOM slide"));
    let reader = DicomReader { slide };
    let handle = Slide::from_source_with_cache_bytes(Box::new(reader), 1024 * 1024);
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let control = crate::ReadControl::default().with_diagnostic_sink(Arc::new(
        move |event: crate::DicomIndexDiagnostic| captured.lock().unwrap().push(event),
    ));

    for _ in 0..2 {
        handle
            .prepare_level_controlled(
                SceneId::new(0),
                SeriesId::new(0),
                LevelIdx::new(0),
                &control,
            )
            .expect("prepare DICOM level");
    }

    let outcomes = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| event.outcome)
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        vec![
            crate::DicomIndexOutcome::BuiltFast {
                mapping: crate::DicomIndexMapping::SingleFrameItems,
            },
            crate::DicomIndexOutcome::Reused,
        ]
    );
}

#[test]
fn controlled_preparation_invokes_diagnostic_sink_after_releasing_index_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prepare-level-reentrant-diagnostics.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let slide = Arc::new(DicomSlide::parse(&path).expect("parse DICOM slide"));
    let image = slide.levels[0].parts[0].clone();
    let reader = DicomReader { slide };
    let handle = Slide::from_source_with_cache_bytes(Box::new(reader), 1024 * 1024);
    let callback_observed_unlocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = Arc::clone(&callback_observed_unlocked);
    let callback_image = image.clone();
    let control = crate::ReadControl::default().with_diagnostic_sink(Arc::new(
        move |_event: crate::DicomIndexDiagnostic| {
            let lock_available = callback_image
                .frame_store
                .encapsulated_frames
                .try_lock()
                .is_ok();
            observed.store(lock_available, std::sync::atomic::Ordering::Release);
        },
    ));

    handle
        .prepare_level_controlled(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(0),
            &control,
        )
        .expect("prepare DICOM level");

    assert!(
        callback_observed_unlocked.load(std::sync::atomic::Ordering::Acquire),
        "diagnostic callbacks must not run while the encapsulated-frame index mutex is held"
    );
}

#[test]
fn controlled_indexing_reports_token_fallback_for_implicit_vr_layout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("implicit-vr-index-fallback.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: uids::IMPLICIT_VR_LITTLE_ENDIAN,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let control = crate::ReadControl::default().with_diagnostic_sink(Arc::new(
        move |event: crate::DicomIndexDiagnostic| captured.lock().unwrap().push(event),
    ));

    let frames = scan_encapsulated_frames_controlled(
        &path,
        uids::IMPLICIT_VR_LITTLE_ENDIAN,
        1,
        Some(&control),
    )
    .expect("token parser should index the implicit-VR encapsulated layout");

    assert_eq!(frames.frame_ranges, vec![0..1]);
    let outcomes = events
        .lock()
        .unwrap()
        .iter()
        .map(|event| event.outcome)
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        vec![
            crate::DicomIndexOutcome::FastPathFallback,
            crate::DicomIndexOutcome::TokenFallback,
        ]
    );
}

#[test]
fn disabled_index_diagnostics_do_not_sample_the_clock() {
    let clock_calls = std::cell::Cell::new(0);
    let started = index_diagnostic_timer_with(Some(&crate::ReadControl::default()), false, || {
        clock_calls.set(clock_calls.get() + 1);
        std::time::Instant::now()
    });

    assert!(started.is_none());
    assert_eq!(clock_calls.get(), 0);
}

#[test]
fn concurrent_frame_index_preparation_reuses_one_complete_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent-prepare-level.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let slide = DicomSlide::parse(&path).expect("parse DICOM slide");
    let image = slide.levels[0].parts[0].clone();
    let workers = (0..8)
        .map(|_| {
            let image = image.clone();
            std::thread::spawn(move || image.ensure_encapsulated_frames().unwrap())
        })
        .collect::<Vec<_>>();
    let indexes = workers
        .into_iter()
        .map(|worker| worker.join().expect("preparation worker did not panic"))
        .collect::<Vec<_>>();

    assert!(
        indexes
            .windows(2)
            .all(|pair| Arc::ptr_eq(&pair[0], &pair[1])),
        "concurrent preparation should publish and reuse one complete index"
    );
}

#[test]
fn cancelled_level_preparation_does_not_publish_an_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel-prepare-level.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let slide = Arc::new(DicomSlide::parse(&path).expect("parse DICOM slide"));
    let image = slide.levels[0].parts[0].clone();
    let reader = DicomReader { slide };
    let cancellation = crate::ReadCancellationToken::new();
    cancellation.cancel();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let control = crate::ReadControl::new(cancellation).with_diagnostic_sink(Arc::new(
        move |event: crate::DicomIndexDiagnostic| captured.lock().unwrap().push(event),
    ));

    let error = reader
        .prepare_level_controlled(
            SceneId::new(0),
            SeriesId::new(0),
            LevelIdx::new(0),
            &control,
        )
        .expect_err("cancelled preparation should stop");

    assert!(matches!(error, WsiError::Cancelled));
    assert!(
        events.lock().unwrap().is_empty(),
        "cancelled preparation must not report an index outcome"
    );
    assert!(
        image
            .frame_store
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none(),
        "cancelled preparation must not publish a partial index"
    );
}

#[test]
fn cancellation_during_frame_index_build_does_not_publish_the_completed_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel-during-index-build.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let slide = DicomSlide::parse(&path).expect("parse DICOM slide");
    let image = slide.levels[0].parts[0].clone();
    let cancellation = crate::ReadCancellationToken::new();
    let control = crate::ReadControl::new(cancellation.clone());

    let error = image
        .ensure_encapsulated_frames_with_builder(Some(&control), || {
            cancellation.cancel();
            Ok(DicomEncapsulatedFrames {
                fragments: vec![DicomFragmentRef {
                    item_offset: 0,
                    payload_offset: 8,
                    len: 4,
                }],
                frame_ranges: std::iter::once(0..1).collect(),
            })
        })
        .expect_err("a cancelled build must not publish its completed candidate");

    assert!(matches!(error, WsiError::Cancelled));
    assert!(
        image
            .frame_store
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none(),
        "cancellation during the build must leave no cached index"
    );
}

#[test]
fn cancellation_during_extended_table_read_does_not_publish_an_index() {
    struct CancellingTableReader {
        inner: std::io::Cursor<Vec<u8>>,
        cancellation: crate::ReadCancellationToken,
    }

    impl std::io::Read for CancellingTableReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            if read > 0 {
                self.cancellation.cancel();
            }
            Ok(read)
        }
    }

    impl std::io::Seek for CancellingTableReader {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel-during-extended-table.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let slide = DicomSlide::parse(&path).expect("parse DICOM slide");
    let image = slide.levels[0].parts[0].clone();
    let cancellation = crate::ReadCancellationToken::new();
    let control = crate::ReadControl::new(cancellation.clone());
    let table_len = 64 * 1024u32;
    let mut reader = CancellingTableReader {
        inner: std::io::Cursor::new(vec![0; 2 * table_len as usize]),
        cancellation,
    };

    let error = image
        .ensure_encapsulated_frames_with_builder(Some(&control), || {
            let _ = read_extended_offset_tables_with_reader(
                &mut reader,
                &path,
                0,
                u64::from(table_len),
                table_len,
                2 * u64::from(table_len),
                Some(&control),
            )?;
            Ok(DicomEncapsulatedFrames {
                fragments: Vec::new(),
                frame_ranges: Vec::new(),
            })
        })
        .expect_err("cancellation between bounded table chunks must stop preparation");

    assert!(matches!(error, WsiError::Cancelled));
    assert!(
        image
            .frame_store
            .encapsulated_frames
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none(),
        "a cancelled extended-table read must not publish an index"
    );
}

#[test]
fn indexed_fragment_header_is_revalidated_before_payload_read() {
    use std::io::{Seek as _, Write as _};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fragment-header-revalidation.dcm");
    write_test_dicom(
        &path,
        TestDicomOptions {
            transfer_syntax: HTJ2K_LOSSLESS_RPCL_TRANSFER_SYNTAX,
            samples_per_pixel: 3,
            photometric_interpretation: "RGB",
            planar_configuration: Some(0),
            pixel_spacing: Some("0.00025\\0.00025"),
            shared_pixel_spacing: None,
            pixel_data: TestPixelData::Encapsulated(vec![0xFF, 0x4F, 0x00, 0xFF, 0xD9]),
            ..TestDicomOptions::native(Vec::new())
        },
    );
    let slide = DicomSlide::parse(&path).expect("parse DICOM slide");
    let image = slide.levels[0].parts[0].clone();
    let frames = image
        .ensure_encapsulated_frames()
        .expect("build the frame index before corrupting the source");
    let item_offset = frames.fragments[0].item_offset;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open fixture for corruption");
    file.seek(std::io::SeekFrom::Start(item_offset))
        .expect("seek to fragment Item header");
    file.write_all(&[0xAA; 4])
        .expect("replace fragment Item tag");
    drop(file);

    let error = image
        .extract_encapsulated_frame(0, 0, 0, 0, false)
        .expect_err("an indexed fragment with a corrupt Item header must not be returned");

    assert!(error
        .to_string()
        .contains("does not match its indexed length"));
}
