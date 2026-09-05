use super::*;
use crate::{Dataset, DatasetId, TileRequest};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Condvar, Mutex,
};
use std::time::{Duration, Instant};

struct OverlappingSource {
    dataset: Dataset,
    reads: [AtomicUsize; 4],
    gate: Mutex<bool>,
    fail: AtomicBool,
    ready: Condvar,
}

impl OverlappingSource {
    fn new(blocked: bool) -> Self {
        Self {
            dataset: crate::test_support::regular_rgb_dataset_for_test(
                DatasetId::new(911),
                "s",
                "series",
                crate::test_support::RegularLevelForTest {
                    dimensions: (4, 1),
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across: 4,
                    tiles_down: 1,
                },
            ),
            reads: std::array::from_fn(|_| AtomicUsize::new(0)),
            fail: AtomicBool::new(false),
            gate: Mutex::new(blocked),
            ready: Condvar::new(),
        }
    }
    fn release(&self) {
        *self.gate.lock().unwrap() = false;
        self.ready.notify_all();
    }
}

impl SlideReader for OverlappingSource {
    fn dataset(&self) -> &Dataset {
        &self.dataset
    }
    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.reads[req.col as usize].fetch_add(1, Ordering::SeqCst);
        let mut gate = self.gate.lock().unwrap();
        while *gate {
            gate = self.ready.wait(gate).unwrap();
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(WsiError::ResourceLimit {
                resource: "test input",
                requested: 4,
                limit: 3,
            });
        }
        CpuTile::from_u8_interleaved(1, 1, 3, ColorSpace::Rgb, vec![req.col as u8; 3])
    }
}

#[test]
fn overlapping_region_batches_load_each_source_tile_once() {
    let source = OverlappingSource::new(true);
    let cache = TileCache::new(1024);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            composite_region_from_source(
                &source,
                Some(&cache),
                &RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (3, 1)),
                16,
            )
            .unwrap()
        });
        let second = scope.spawn(|| {
            composite_region_from_source(
                &source,
                Some(&cache),
                &RegionRequest::new(0usize, 0usize, 0u32, (1, 0), (3, 1)),
                16,
            )
            .unwrap()
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while cache.stats().misses < 6 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let both_planned = cache.stats().misses >= 6;
        source.release();
        assert!(
            both_planned,
            "both overlapping requests must reach the cold cache"
        );
        assert_eq!(
            first.join().unwrap().as_u8().unwrap(),
            &[0, 0, 0, 1, 1, 1, 2, 2, 2]
        );
        assert_eq!(
            second.join().unwrap().as_u8().unwrap(),
            &[1, 1, 1, 2, 2, 2, 3, 3, 3]
        );
    });
    assert_eq!(
        source.reads.each_ref().map(|n| n.load(Ordering::SeqCst)),
        [1; 4],
        "overlapping batches must share source work, including opposite ownership order"
    );
    assert_eq!(cache.stats().puts, 4);
}

#[test]
fn failed_shared_region_reads_preserve_error_types_and_can_retry() {
    for streaming in [false, true] {
        let source = OverlappingSource::new(true);
        source.fail.store(true, Ordering::SeqCst);
        let cache = TileCache::new(1024);
        let req = RegionRequest::new(0usize, 0usize, 0u32, (0, 0), (1, 1));
        let read = || {
            if streaming {
                composite_region_from_source_streaming(&source, Some(&cache), &req, 16)
            } else {
                composite_region_from_source(&source, Some(&cache), &req, 16)
            }
        };
        std::thread::scope(|scope| {
            let a = scope.spawn(read);
            let b = scope.spawn(read);
            let deadline = Instant::now() + Duration::from_secs(2);
            while cache.stats().misses < 2 && Instant::now() < deadline {
                std::thread::yield_now();
            }
            let both_planned = cache.stats().misses >= 2;
            source.release();
            assert!(both_planned);
            for result in [a.join().unwrap(), b.join().unwrap()] {
                assert!(matches!(
                    result,
                    Err(WsiError::ResourceLimit {
                        resource: "test input",
                        requested: 4,
                        limit: 3
                    })
                ));
            }
        });
        assert_eq!(cache.stats().puts, 0);
        source.fail.store(false, Ordering::SeqCst);
        assert_eq!(read().unwrap().as_u8().unwrap(), &[0, 0, 0]);
        assert_eq!(cache.stats().puts, 1);
    }
}
