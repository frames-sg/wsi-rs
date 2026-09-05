use super::*;
use std::io::{self, Cursor};

struct ObservedReader {
    input: Cursor<Vec<u8>>,
    reads: usize,
    max_read: usize,
    largest_request: usize,
}

impl Read for ObservedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        self.largest_request = self.largest_request.max(buf.len());
        let len = buf.len().min(self.max_read);
        self.input.read(&mut buf[..len])
    }
}

impl Seek for ObservedReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.input.seek(position)
    }
}

fn index_input(records: i32, max_read: usize) -> ObservedReader {
    // Two hierarchy roots, a populated page, then an empty reduced-level page.
    // Repeated coordinates intentionally exercise the existing last-record rule.
    let empty_page = 32 + records * 16;
    let mut words = vec![8, 16, 0, 24, 0, empty_page, records, 0];
    for offset in 0..records {
        words.extend([0, offset, 1, 0]);
    }
    words.extend([0, 0]);
    ObservedReader {
        input: Cursor::new(words.into_iter().flat_map(i32::to_le_bytes).collect()),
        reads: 0,
        max_read,
        largest_request: 0,
    }
}

fn parse_index(input: &mut ObservedReader) -> Result<Vec<MiraxLevelBuilder>, WsiError> {
    let mut levels: Vec<_> = (0..2)
        .map(|_| MiraxLevelBuilder {
            dimensions: (1, 1),
            downsample: 1.0,
            image_format: MiraxImageFormat::Jpeg,
            raw_image_width: 1,
            raw_image_height: 1,
            tile_width: 1.0,
            tile_height: 1.0,
            tile_advance_x: 1.0,
            tile_advance_y: 1.0,
            tiles: HashMap::new(),
            descriptors: Vec::new(),
            extra_tiles: (0, 0, 0, 0),
        })
        .collect();
    let params = [SlideZoomLevelParams {
        image_concat: 1,
        tile_count_divisor: 1,
        tiles_per_image: 1,
        positions_per_tile: 1,
        tile_advance_x: 1.0,
        tile_advance_y: 1.0,
    }; 2];
    process_hier_data_pages_from_indexfile(MiraxIndexBuildContext {
        path: Path::new("slide.mrxs"),
        index_file: input,
        index_path: Path::new("Index.dat"),
        seek_location: 0,
        datafile_paths: &[PathBuf::from("Data0000.dat")],
        images: (1, 1),
        image_divisions: 1,
        params: &params,
        levels: &mut levels,
        slide_positions: &[0, 0],
        quickhash: &mut Quickhash1::new(),
        quickhash_files: &mut HashMap::new(),
        open_budget: OpenBudget::new(crate::SlideLimits::default()).as_ref(),
    })?;
    Ok(levels)
}

#[test]
fn hierarchy_index_reads_are_bounded_batches_with_unchanged_record_order() {
    let mut input = index_input(1024, usize::MAX);
    let levels = parse_index(&mut input).unwrap();
    assert_eq!(levels[0].descriptors.len(), 1024);
    for (i, tile) in levels[0].descriptors.iter().enumerate() {
        assert_eq!(tile.image.record.offset, i as u64);
        assert_eq!(tile.image.id, i as u32);
    }
    assert_eq!(levels[0].tiles[&(0, 0)].tiff_tile_index, Some(1023));
    assert!(levels[1].tiles.is_empty());
    assert!(input.largest_request <= 8192);
    assert!(
        input.reads <= 16,
        "index used {} underlying reads",
        input.reads
    );
}

#[test]
fn hierarchy_index_handles_short_reads_backward_seeks_and_truncation() {
    let mut input = index_input(513, 3);
    let levels = parse_index(&mut input).unwrap();
    assert_eq!(levels[0].descriptors.len(), 513);
    assert!(levels[1].tiles.is_empty());

    let mut input = index_input(513, 3);
    input.input.get_mut().pop();
    assert!(matches!(
        parse_index(&mut input),
        Err(WsiError::IoWithPath { path, source })
            if path == Path::new("Index.dat") && source.kind() == io::ErrorKind::UnexpectedEof
    ));
}
