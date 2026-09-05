use super::*;
use std::io::{self, Cursor};

struct ObservedIndex {
    input: Cursor<Vec<u8>>,
    reads: usize,
    seeks: usize,
    max_read: usize,
    largest_read: usize,
}

impl Read for ObservedIndex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        self.largest_read = self.largest_read.max(buf.len());
        let len = buf.len().min(self.max_read);
        self.input.read(&mut buf[..len])
    }
}

impl Seek for ObservedIndex {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.seeks += 1;
        self.input.seek(pos)
    }
}

fn fixture(count: u32, max_read: usize) -> (ObservedIndex, EtsHeader) {
    let mut bytes = Vec::new();
    let payload_offset = u64::from(count) * 36;
    for col in 0..count {
        for value in [0, col, 0, 0, 0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&payload_offset.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
    }
    bytes.push(0);
    (
        ObservedIndex {
            input: Cursor::new(bytes),
            reads: 0,
            seeks: 0,
            max_read,
            largest_read: 0,
        },
        EtsHeader {
            file_len: payload_offset + 1,
            n_dimensions: 4,
            used_chunk_offset: 0,
            n_used_chunks: count,
            use_pyramid: true,
            tile_width: 16,
            tile_height: 12,
            sample_type: crate::SampleType::Uint8,
            samples_per_pixel: 3,
            background: vec![7, 11, 13],
        },
    )
}

#[test]
fn ets_index_batches_reads_and_padding_skips_without_changing_tiles() {
    let (mut input, header) = fixture(1024, usize::MAX);
    let budget = OpenBudget::new(crate::SlideLimits::default());
    let index = EtsIndex::read(&mut input, Path::new("frame_t.ets"), &budget, &header).unwrap();
    assert_eq!(index.tiles.len(), 1024);
    for col in 0..1024 {
        let tile = index.tiles[&key_from_coords(&[col, 0, 0, 0], true).unwrap()];
        assert_eq!(tile.offset, 1024 * 36);
        assert_eq!(tile.byte_count, 1);
    }
    assert_eq!(
        index.axes(Path::new("frame_t.ets")).unwrap(),
        AxesShape::default()
    );
    assert!(input.largest_read <= 8192);
    assert!(input.reads <= 8, "{} underlying reads", input.reads);
    assert!(input.seeks <= 2, "{} underlying seeks", input.seeks);
}

#[test]
fn ets_index_short_reads_truncation_and_duplicate_rejection_are_preserved() {
    let budget = || OpenBudget::new(crate::SlideLimits::default());
    let path = Path::new("frame_t.ets");
    let (mut input, header) = fixture(300, 3);
    let index = EtsIndex::read(&mut input, path, &budget(), &header).unwrap();
    assert_eq!(index.tiles.len(), 300);

    let (mut input, header) = fixture(300, 3);
    input.input.get_mut().truncate(300 * 36 - 5);
    assert!(EtsIndex::read(&mut input, path, &budget(), &header).is_err());

    let (mut input, header) = fixture(2, usize::MAX);
    input.input.get_mut()[40..44].copy_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        EtsIndex::read(&mut input, path, &budget(), &header),
        Err(WsiError::InvalidSlide { message, .. }) if message == "duplicate ETS tile coordinates"
    ));
}
