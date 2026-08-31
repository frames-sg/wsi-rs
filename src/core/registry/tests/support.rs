use super::super::*;
use crate::test_support::{regular_rgb_dataset_for_test, RegularLevelForTest};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub(super) struct MockSource {
    ds: Dataset,
}

impl MockSource {
    pub(super) fn new() -> Self {
        Self {
            ds: regular_rgb_dataset_for_test(
                DatasetId::new(1),
                "s0",
                "ser0",
                RegularLevelForTest {
                    dimensions: (512, 512),
                    tile_width: 256,
                    tile_height: 256,
                    tiles_across: 2,
                    tiles_down: 2,
                },
            ),
        }
    }

    fn tile_color(col: i64, row: i64) -> [u8; 3] {
        match (col, row) {
            (0, 0) => [255, 0, 0],     // red
            (1, 0) => [0, 255, 0],     // green
            (0, 1) => [0, 0, 255],     // blue
            (1, 1) => [255, 255, 255], // white
            _ => [0, 0, 0],            // black (out of range)
        }
    }
}

impl SlideReader for MockSource {
    fn dataset(&self) -> &Dataset {
        &self.ds
    }
    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        let [r, g, b] = MockSource::tile_color(req.col, req.row);
        let mut data = vec![0u8; 256 * 256 * 3];
        for pixel in data.chunks_exact_mut(3) {
            pixel[0] = r;
            pixel[1] = g;
            pixel[2] = b;
        }
        Ok(CpuTile {
            width: 256,
            height: 256,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(data),
        })
    }
}

pub(super) struct CountingSource {
    ds: Dataset,
    tile_reads: Arc<AtomicUsize>,
}

impl CountingSource {
    pub(super) fn new(dataset_id: DatasetId, tile_reads: Arc<AtomicUsize>) -> Self {
        Self {
            ds: regular_rgb_dataset_for_test(
                dataset_id,
                "s0",
                "ser0",
                RegularLevelForTest {
                    dimensions: (256, 256),
                    tile_width: 256,
                    tile_height: 256,
                    tiles_across: 1,
                    tiles_down: 1,
                },
            ),
            tile_reads,
        }
    }
}

impl SlideReader for CountingSource {
    fn dataset(&self) -> &Dataset {
        &self.ds
    }

    fn read_tile_cpu(&self, _req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.tile_reads.fetch_add(1, Ordering::SeqCst);
        Ok(CpuTile {
            width: 256,
            height: 256,
            channels: 3,
            color_space: ColorSpace::Rgb,
            layout: CpuTileLayout::Interleaved,
            data: CpuTileData::u8(vec![9u8; 256 * 256 * 3]),
        })
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        Err(WsiError::AssociatedImageNotFound(name.into()))
    }
}

pub(super) struct BatchCountingSource {
    inner: MockSource,
    tile_reads: Arc<AtomicUsize>,
    batch_reads: Arc<AtomicUsize>,
    batch_tile_count: Arc<AtomicUsize>,
}

impl BatchCountingSource {
    pub(super) fn new(
        tile_reads: Arc<AtomicUsize>,
        batch_reads: Arc<AtomicUsize>,
        batch_tile_count: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            inner: MockSource::new(),
            tile_reads,
            batch_reads,
            batch_tile_count,
        }
    }
}

impl SlideReader for BatchCountingSource {
    fn dataset(&self) -> &Dataset {
        self.inner.dataset()
    }

    fn read_tile_cpu(&self, req: &TileRequest) -> Result<CpuTile, WsiError> {
        self.tile_reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read_tile_cpu(req)
    }

    fn read_tiles_cpu(&self, reqs: &[TileRequest]) -> Result<Vec<CpuTile>, WsiError> {
        self.batch_reads.fetch_add(1, Ordering::SeqCst);
        self.batch_tile_count
            .fetch_add(reqs.len(), Ordering::SeqCst);
        reqs.iter()
            .map(|req| self.inner.read_tile_cpu(req))
            .collect()
    }

    fn read_associated(&self, name: &str) -> Result<CpuTile, WsiError> {
        self.inner.read_associated(name)
    }
}
