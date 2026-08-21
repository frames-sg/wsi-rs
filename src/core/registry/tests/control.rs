use super::super::*;
use super::support::{BatchCountingSource, MockSource};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn read_tile_rejects_wrong_batch_cardinality() {
    struct BadBatchReader {
        inner: MockSource,
    }

    impl SlideReader for BadBatchReader {
        fn dataset(&self) -> &Dataset {
            self.inner.dataset()
        }

        fn read_tiles(
            &self,
            _reqs: &[TileRequest],
            _output: TileOutputPreference,
        ) -> Result<Vec<TilePixels>, WsiError> {
            Ok(vec![
                TilePixels::Cpu(self.inner.read_tile_cpu(&TileRequest {
                    scene: 0usize.into(),
                    series: 0usize.into(),
                    level: 0u32.into(),
                    plane: PlaneSelection::default().into(),
                    col: 0,
                    row: 0,
                })?),
                TilePixels::Cpu(self.inner.read_tile_cpu(&TileRequest {
                    scene: 0usize.into(),
                    series: 0usize.into(),
                    level: 0u32.into(),
                    plane: PlaneSelection::default().into(),
                    col: 1,
                    row: 0,
                })?),
            ])
        }

        fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
            self.inner.read_tile_cpu(req)
        }

        fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
            self.inner.read_associated(name)
        }
    }

    let reader = BadBatchReader {
        inner: MockSource::new(),
    };
    let err = reader
        .read_tile(
            &TileRequest {
                scene: 0usize.into(),
                series: 0usize.into(),
                level: 0u32.into(),
                plane: PlaneSelection::default().into(),
                col: 0,
                row: 0,
            },
            TileOutputPreference::cpu(),
        )
        .expect_err("single read must reject extra batch outputs");
    assert!(matches!(err, WsiError::TileRead { .. }));
    assert!(err.to_string().contains("returned 2 tiles"));
}

struct CancellingSource {
    inner: MockSource,
    token: crate::ReadCancellationToken,
    batch_reads: Arc<AtomicUsize>,
    batch_tile_count: Arc<AtomicUsize>,
}

struct FailingCancellingSource {
    inner: MockSource,
    token: crate::ReadCancellationToken,
}

impl SlideReader for FailingCancellingSource {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        _output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.token.cancel();
        let req = reqs.first().expect("test submits one tile");
        Err(WsiError::TileRead {
            col: req.col,
            row: req.row,
            level: req.level.get(),
            reason: "source failed while cancellation was requested".into(),
        })
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_tile_cpu(req)
    }
}

impl SlideReader for CancellingSource {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn read_tiles(
        &self,
        reqs: &[TileRequest],
        _output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        self.batch_reads.fetch_add(1, Ordering::SeqCst);
        self.batch_tile_count
            .fetch_add(reqs.len(), Ordering::SeqCst);
        self.token.cancel();
        reqs.iter()
            .map(|req| self.inner.read_tile_cpu(req).map(TilePixels::Cpu))
            .collect()
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_tile_cpu(req)
    }
}

#[test]
fn controlled_tile_reads_preserve_one_full_batch_and_check_cancellation_afterward() {
    let token = crate::ReadCancellationToken::new();
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let batch_tile_count = Arc::new(AtomicUsize::new(0));
    let source: Box<dyn SlideReader> = Box::new(CancellingSource {
        inner: MockSource::new(),
        token: token.clone(),
        batch_reads: Arc::clone(&batch_reads),
        batch_tile_count: Arc::clone(&batch_tile_count),
    });
    let slide = Slide::from_source(source, Arc::new(TileCache::new(1024)));
    let requests = [
        TileRequest::new(0usize, 0usize, 0, 0, 0),
        TileRequest::new(0usize, 0usize, 0, 1, 0),
    ];

    let error = slide
        .read_tiles_controlled(
            &requests,
            TileOutputPreference::cpu(),
            &crate::ReadControl::new(token),
        )
        .unwrap_err();

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(batch_reads.load(Ordering::SeqCst), 1);
    assert_eq!(batch_tile_count.load(Ordering::SeqCst), requests.len());
}

#[test]
fn controlled_read_reports_terminal_cancellation_when_source_also_fails() {
    let token = crate::ReadCancellationToken::new();
    let source: Box<dyn SlideReader> = Box::new(FailingCancellingSource {
        inner: MockSource::new(),
        token: token.clone(),
    });
    let slide = Slide::from_source(source, Arc::new(TileCache::new(1024)));

    let error = slide
        .read_tiles_controlled(
            &[TileRequest::new(0usize, 0usize, 0, 0, 0)],
            TileOutputPreference::cpu(),
            &crate::ReadControl::new(token),
        )
        .expect_err("cancellation must take precedence over a simultaneous source error");

    assert!(matches!(error, WsiError::Cancelled));
}

#[test]
fn default_controlled_read_preserves_batch_order_and_cardinality() {
    let tile_reads = Arc::new(AtomicUsize::new(0));
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let batch_tile_count = Arc::new(AtomicUsize::new(0));
    let source: Box<dyn SlideReader> = Box::new(BatchCountingSource::new(
        Arc::clone(&tile_reads),
        Arc::clone(&batch_reads),
        Arc::clone(&batch_tile_count),
    ));
    let slide = Slide::from_source(source, Arc::new(TileCache::new(1024)));
    let requests = [
        TileRequest::new(0usize, 0usize, 0, 1, 0),
        TileRequest::new(0usize, 0usize, 0, 0, 0),
        TileRequest::new(0usize, 0usize, 0, 0, 1),
    ];

    let tiles = slide
        .read_tiles_controlled(
            &requests,
            TileOutputPreference::cpu(),
            &crate::ReadControl::default(),
        )
        .expect("controlled batch");

    assert_eq!(batch_reads.load(Ordering::SeqCst), 1);
    assert_eq!(batch_tile_count.load(Ordering::SeqCst), requests.len());
    assert_eq!(tile_reads.load(Ordering::SeqCst), 0);
    let first_rgb = tiles
        .iter()
        .map(|tile| {
            #[allow(unreachable_patterns)]
            match tile {
                TilePixels::Cpu(tile) => &tile.data.as_u8().expect("RGB8 tile")[..3],
                TilePixels::Device(_) => panic!("test source returns CPU tiles"),
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        first_rgb,
        vec![&[0, 255, 0][..], &[255, 0, 0][..], &[0, 0, 255][..]]
    );
}

struct WrongCardinalitySource {
    inner: MockSource,
}

impl SlideReader for WrongCardinalitySource {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn read_tiles(
        &self,
        _reqs: &[TileRequest],
        _output: TileOutputPreference,
    ) -> Result<Vec<TilePixels>, WsiError> {
        Ok(Vec::new())
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.inner.read_tile_cpu(req)
    }
}

#[test]
fn default_controlled_read_reports_wrong_cardinality_as_backend_contract() {
    let source: Box<dyn SlideReader> = Box::new(WrongCardinalitySource {
        inner: MockSource::new(),
    });
    let slide = Slide::from_source(source, Arc::new(TileCache::new(1024)));
    let requests = [
        TileRequest::new(0usize, 0usize, 0, 0, 0),
        TileRequest::new(0usize, 0usize, 0, 1, 0),
    ];

    let error = slide
        .read_tiles_controlled(
            &requests,
            TileOutputPreference::cpu(),
            &crate::ReadControl::default(),
        )
        .expect_err("wrong cardinality must fail");

    assert!(matches!(
        error,
        WsiError::BackendContract {
            expected: 2,
            actual: 0,
            ..
        }
    ));
}

#[test]
fn default_controlled_read_checks_cancellation_before_batch_admission() {
    let tile_reads = Arc::new(AtomicUsize::new(0));
    let batch_reads = Arc::new(AtomicUsize::new(0));
    let batch_tile_count = Arc::new(AtomicUsize::new(0));
    let source: Box<dyn SlideReader> = Box::new(BatchCountingSource::new(
        Arc::clone(&tile_reads),
        Arc::clone(&batch_reads),
        Arc::clone(&batch_tile_count),
    ));
    let slide = Slide::from_source(source, Arc::new(TileCache::new(1024)));
    let token = crate::ReadCancellationToken::new();
    token.cancel();

    let error = slide
        .read_tiles_controlled(
            &[TileRequest::new(0usize, 0usize, 0, 0, 0)],
            TileOutputPreference::cpu(),
            &crate::ReadControl::new(token),
        )
        .expect_err("pre-cancelled batch must not be admitted");

    assert!(matches!(error, WsiError::Cancelled));
    assert_eq!(batch_reads.load(Ordering::SeqCst), 0);
    assert_eq!(batch_tile_count.load(Ordering::SeqCst), 0);
    assert_eq!(tile_reads.load(Ordering::SeqCst), 0);
}

struct ControlledOverrideSource {
    inner: MockSource,
    controlled_reads: Arc<AtomicUsize>,
}

impl SlideReader for ControlledOverrideSource {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn read_tiles_controlled(
        &self,
        reqs: &[TileRequest],
        _output: TileOutputPreference,
        control: &crate::ReadControl,
    ) -> Result<Vec<TilePixels>, WsiError> {
        control.check_cancelled()?;
        self.controlled_reads.fetch_add(1, Ordering::SeqCst);
        reqs.iter()
            .map(|req| self.inner.read_tile_cpu(req).map(TilePixels::Cpu))
            .collect()
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        panic!("read_tile_controlled must use the controlled batch path")
    }
}

#[test]
fn read_tile_controlled_delegates_to_controlled_batch_path() {
    let controlled_reads = Arc::new(AtomicUsize::new(0));
    let source: Box<dyn SlideReader> = Box::new(ControlledOverrideSource {
        inner: MockSource::new(),
        controlled_reads: Arc::clone(&controlled_reads),
    });
    let slide = Slide::from_source(source, Arc::new(TileCache::new(1024)));

    let tile = slide
        .read_tile_controlled(
            &TileRequest::new(0usize, 0usize, 0, 1, 0),
            TileOutputPreference::cpu(),
            &crate::ReadControl::default(),
        )
        .expect("controlled single tile");

    assert_eq!(controlled_reads.load(Ordering::SeqCst), 1);
    #[allow(unreachable_patterns)]
    let tile = match tile {
        TilePixels::Cpu(tile) => tile,
        TilePixels::Device(_) => panic!("test source returns CPU tiles"),
    };
    assert_eq!(&tile.data.as_u8().expect("RGB8 tile")[..3], &[0, 255, 0]);
}
