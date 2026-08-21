use super::super::*;

pub(super) struct TestNdpiJpegLayout {
    pub(super) ifd_id: IfdId,
    pub(super) dimensions: (u32, u32),
    pub(super) virtual_tile: (u32, u32),
    pub(super) tile_grid: (u32, u32),
    pub(super) jpeg_header: Vec<u8>,
    pub(super) strip_byte_count: u64,
}

pub(super) const TEST_NDPI_RESTART_COLORS: [[u8; 3]; 4] =
    [[240, 20, 20], [20, 220, 20], [20, 20, 230], [220, 220, 30]];

pub(super) fn build_test_ndpi_restart_reader(zero_sof_dimensions: bool) -> TiffPixelReader {
    let (file, jpeg_header, strip_byte_count) = build_ndpi_scan_data_tiff_from_blobs(
        128,
        16,
        &TEST_NDPI_RESTART_COLORS,
        zero_sof_dimensions,
    );
    let container = Arc::new(TiffContainer::open(file.path()).unwrap());
    let ifd_id = *container.top_ifds().first().unwrap();
    let layout = build_test_ndpi_layout_from_header(TestNdpiJpegLayout {
        ifd_id,
        dimensions: (128, 16),
        virtual_tile: (64, 8),
        tile_grid: (2, 2),
        jpeg_header,
        strip_byte_count,
    });
    TiffPixelReader::new(container, layout)
}

pub(super) fn read_test_ndpi_level0_tile(reader: &TiffPixelReader, col: i64, row: i64) -> CpuTile {
    reader
        .read_tile_cpu(&TileRequest {
            scene: 0usize.into(),
            series: 0usize.into(),
            level: 0u32.into(),
            plane: PlaneSelection::default().into(),
            col,
            row,
        })
        .unwrap()
}

pub(super) fn build_test_ndpi_layout_from_header(spec: TestNdpiJpegLayout) -> DatasetLayout {
    build_test_ndpi_layout_from_header_with_strip_offset(spec, 8)
}

pub(super) fn build_test_ndpi_layout_from_header_with_strip_offset(
    spec: TestNdpiJpegLayout,
    strip_offset: u64,
) -> DatasetLayout {
    build_test_ndpi_layout_from_header_with_restart_interval(spec, strip_offset, 8)
}

pub(super) fn build_test_ndpi_layout_from_header_with_restart_interval(
    spec: TestNdpiJpegLayout,
    strip_offset: u64,
    restart_interval: u16,
) -> DatasetLayout {
    let (width, height) = spec.dimensions;
    let (virtual_tile_width, virtual_tile_height) = spec.virtual_tile;
    let (tiles_across, tiles_down) = spec.tile_grid;
    single_series_layout(
        DatasetId::new(12),
        vec![whole_level(
            (u64::from(width), u64::from(height)),
            1.0,
            (virtual_tile_width, virtual_tile_height),
        )],
        HashMap::from([(
            tile_source_key(0),
            TileSource::NdpiJpeg {
                ifd_id: spec.ifd_id,
                jpeg_header: spec.jpeg_header,
                mcu_starts_tag: 65426,
                tiles_across,
                tiles_down,
                restart_interval,
                strip_offset,
                strip_byte_count: spec.strip_byte_count,
            },
        )]),
    )
}
