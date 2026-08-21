use super::fixtures::{main_fixture, metadata_xml, write_fixture, SubblockSpec};
use crate::formats::zeiss::slide::ZeissSlide;
use crate::formats::zeiss::tiles::{
    bitmap_from_raw_uncompressed_subblock, bitmap_to_sample_buffer,
    blit_raw_uncompressed_rgb_subblock, blit_rgb_sample, blit_tile, RgbSample,
};
use crate::TileLayout;
use czi_rs::{
    Bitmap, CompressionMode, Coordinate, DirectorySubBlockInfo, IntRect, IntSize, PixelType,
    RawSubBlock,
};
use j2k_core::BackendRequest;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[cfg(unix)]
fn replace_path_and_truncate_open_fixture(fixture: &tempfile::NamedTempFile) -> std::path::PathBuf {
    let source_path = fixture.path();
    let replacement = std::fs::read(source_path).expect("read replacement CZI bytes");
    let open_inode_path = source_path.with_extension("open-inode.czi");
    std::fs::rename(source_path, &open_inode_path).expect("detach open CZI inode from source path");
    std::fs::write(source_path, replacement).expect("replace path with valid CZI bytes");
    fixture
        .as_file()
        .set_len(0)
        .expect("truncate inode held by parsed CZI reader");
    open_inode_path
}

fn raw(
    pixel_type: PixelType,
    compression: CompressionMode,
    size: (u32, u32),
    data: Vec<u8>,
) -> RawSubBlock {
    RawSubBlock {
        info: DirectorySubBlockInfo {
            index: 0,
            file_position: 0,
            file_part: 0,
            pixel_type,
            compression,
            coordinate: Coordinate::new(),
            rect: IntRect::new(0, 0, size.0 as i32, size.1 as i32),
            stored_size: IntSize {
                w: size.0,
                h: size.1,
            },
            m_index: None,
            pyramid_type: None,
        },
        metadata: Vec::new(),
        data,
        attachment: Vec::new(),
    }
}

#[test]
fn bitmap_conversion_normalizes_bgr_bgra_and_u16_channels() {
    let bgr = Bitmap::new(PixelType::Bgr24, 2, 1, vec![3, 2, 1, 6, 5, 4]).expect("BGR bitmap");
    let tile = bitmap_to_sample_buffer(bgr).expect("BGR conversion");
    assert_eq!(tile.data.as_u8(), Some([1, 2, 3, 4, 5, 6].as_slice()));

    let bgra = Bitmap::new(PixelType::Bgra32, 1, 1, vec![9, 8, 7, 200]).expect("BGRA bitmap");
    let tile = bitmap_to_sample_buffer(bgra).expect("BGRA conversion");
    assert_eq!(tile.data.as_u8(), Some([7, 8, 9].as_slice()));

    let bgr48 = Bitmap::new(PixelType::Bgr48, 1, 1, vec![1, 0, 2, 0, 3, 0]).expect("BGR48 bitmap");
    let tile = bitmap_to_sample_buffer(bgr48).expect("BGR48 conversion");
    assert_eq!(tile.data.as_u16(), Some([3, 2, 1].as_slice()));

    let gray = Bitmap::new(PixelType::Gray8, 1, 1, vec![1]).expect("gray bitmap");
    let error = bitmap_to_sample_buffer(gray).expect_err("Gray8 is not RGB-compatible");
    assert!(error.to_string().contains("unsupported Zeiss pixel type"));
}

#[test]
fn raw_uncompressed_bitmap_normalizes_short_and_long_payloads() {
    let short = raw(
        PixelType::Bgr24,
        CompressionMode::UnCompressed,
        (2, 1),
        vec![1, 2, 3],
    );
    let bitmap = bitmap_from_raw_uncompressed_subblock(&short).expect("pad short bitmap");
    assert_eq!(bitmap.data, vec![1, 2, 3, 0, 0, 0]);

    let long = raw(
        PixelType::Bgr24,
        CompressionMode::UnCompressed,
        (1, 1),
        vec![1, 2, 3, 4, 5, 6],
    );
    let bitmap = bitmap_from_raw_uncompressed_subblock(&long).expect("truncate long bitmap");
    assert_eq!(bitmap.data, vec![1, 2, 3]);

    let compressed = raw(
        PixelType::Bgr24,
        CompressionMode::Zstd0,
        (1, 1),
        vec![1, 2, 3],
    );
    let error = bitmap_from_raw_uncompressed_subblock(&compressed)
        .expect_err("compressed payload must not use raw path");
    assert!(error.to_string().contains("unsupported Zeiss compression"));
}

#[test]
fn bitmap_blit_clips_edges_ignores_disjoint_tiles_and_rejects_type_mismatch() {
    let mut destination = Bitmap::zeros(PixelType::Bgr24, 2, 2).expect("destination");
    let source = Bitmap::new(
        PixelType::Bgr24,
        2,
        2,
        vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    )
    .expect("source");
    blit_tile(&mut destination, &source, 1, 1).expect("clipped blit");
    assert_eq!(&destination.data[9..12], &[1, 2, 3]);
    let before = destination.data.clone();
    blit_tile(&mut destination, &source, 10, 10).expect("disjoint blit");
    assert_eq!(destination.data, before);

    let gray = Bitmap::new(PixelType::Gray8, 1, 1, vec![1]).expect("gray source");
    let error = blit_tile(&mut destination, &gray, 0, 0).expect_err("mismatched blit");
    assert!(error.to_string().contains("mismatched pixel types"));
}

#[test]
fn rgb_sample_blit_clips_negative_offsets_and_ignores_disjoint_input() {
    let mut destination = vec![0; 3 * 2 * 3];
    blit_rgb_sample(
        &mut destination,
        (3, 2),
        RgbSample {
            width: 2,
            height: 1,
            data: &[1, 2, 3, 4, 5, 6],
        },
        (-1, 1),
    )
    .expect("negative-offset RGB blit");
    assert_eq!(&destination[9..12], &[4, 5, 6]);
    let before = destination.clone();
    blit_rgb_sample(
        &mut destination,
        (3, 2),
        RgbSample {
            width: 1,
            height: 1,
            data: &[7, 8, 9],
        },
        (4, 4),
    )
    .expect("disjoint RGB blit");
    assert_eq!(destination, before);
}

#[test]
fn direct_raw_blit_converts_bgr_and_bgra_and_validates_payloads() {
    let mut destination = vec![0; 2 * 3];
    blit_raw_uncompressed_rgb_subblock(
        &mut destination,
        2,
        1,
        &raw(
            PixelType::Bgr24,
            CompressionMode::UnCompressed,
            (1, 1),
            vec![3, 2, 1],
        ),
        0,
        0,
    )
    .expect("BGR direct blit");
    blit_raw_uncompressed_rgb_subblock(
        &mut destination,
        2,
        1,
        &raw(
            PixelType::Bgra32,
            CompressionMode::UnCompressed,
            (1, 1),
            vec![6, 5, 4, 99],
        ),
        1,
        0,
    )
    .expect("BGRA direct blit");
    assert_eq!(destination, vec![1, 2, 3, 4, 5, 6]);

    let before = destination.clone();
    blit_raw_uncompressed_rgb_subblock(
        &mut destination,
        2,
        1,
        &raw(
            PixelType::Bgr24,
            CompressionMode::UnCompressed,
            (1, 1),
            vec![3, 2, 1],
        ),
        4,
        0,
    )
    .expect("disjoint direct blit");
    assert_eq!(destination, before);

    let error = blit_raw_uncompressed_rgb_subblock(
        &mut destination,
        2,
        1,
        &raw(
            PixelType::Bgr24,
            CompressionMode::UnCompressed,
            (1, 1),
            vec![1, 2],
        ),
        0,
        0,
    )
    .expect_err("truncated raw payload");
    assert!(error.to_string().contains("shorter than expected"));

    let error = blit_raw_uncompressed_rgb_subblock(
        &mut destination,
        2,
        1,
        &raw(
            PixelType::Gray8,
            CompressionMode::UnCompressed,
            (1, 1),
            vec![1],
        ),
        0,
        0,
    )
    .expect_err("unsupported direct pixel type");
    assert!(error.to_string().contains("unsupported Zeiss direct blit"));
}

#[test]
fn parsed_slide_returns_blank_for_empty_and_nonintersecting_local_candidates() {
    let fixture = main_fixture();
    let mut slide = ZeissSlide::parse(fixture.path()).expect("parse generated CZI");
    slide.canvas_level_tile_subblocks[0].clear();
    let blank = slide
        .read_tile(0, 0, 0, 0, 0, BackendRequest::Cpu)
        .expect("blank tile for empty candidate map");
    assert!(blank
        .data
        .as_u8()
        .expect("blank RGB")
        .iter()
        .all(|byte| *byte == 0));

    let mut slide = ZeissSlide::parse(fixture.path()).expect("parse generated CZI again");
    slide.subblock_origin = (-10_000, -10_000);
    let blank = slide
        .read_tile(0, 0, 0, 0, 0, BackendRequest::Cpu)
        .expect("blank tile for nonintersecting candidates");
    assert!(blank
        .data
        .as_u8()
        .expect("blank RGB")
        .iter()
        .all(|byte| *byte == 0));
}

#[test]
fn parsed_slide_reports_corrupt_candidate_indices_with_context() {
    let fixture = main_fixture();
    let mut slide = ZeissSlide::parse(fixture.path()).expect("parse generated CZI");
    slide.canvas_level_tile_subblocks[0]
        .get_mut(&(0, 0))
        .expect("level-zero candidate list")[0] = usize::MAX;
    let error = slide
        .read_tile(0, 0, 0, 0, 0, BackendRequest::Cpu)
        .expect_err("corrupt local candidate index");
    assert!(error.to_string().contains("subblock index"));

    let mut slide = ZeissSlide::parse(fixture.path()).expect("parse generated CZI once more");
    slide.canvas_level_subblocks[0][0] = usize::MAX;
    let error = slide
        .scene_level_image(0, 0)
        .expect_err("corrupt level candidate index");
    assert!(error.to_string().contains("subblock index"));
}

#[test]
fn missing_reduced_subblocks_fall_back_to_resizing_the_base_level() {
    let fixture = main_fixture();
    let mut slide = ZeissSlide::parse(fixture.path()).expect("parse generated CZI");
    slide.canvas_level_subblocks[1].clear();
    slide.canvas_level_tile_subblocks[1].clear();
    let reduced = slide
        .scene_level_image(0, 1)
        .expect("resize base level for missing reduced subblocks");
    assert_eq!((reduced.width, reduced.height), (2, 1));
    assert_eq!(reduced.channels, 3);
}

#[test]
fn compressed_level_zero_declines_unsafe_fallback_without_decoding() {
    let mut compressed = SubblockSpec::bgr24(0, 0, 1, 1, Vec::new());
    compressed.compression = 5;
    let fixture = write_fixture(&[compressed], &[], &metadata_xml(1, 1));
    let slide = ZeissSlide::parse(fixture.path()).expect("parse compressed CZI");
    let error = slide
        .read_tile(0, 0, 0, 0, 0, BackendRequest::Cpu)
        .expect_err("compressed level zero cannot use direct composition");
    assert!(error.to_string().contains("direct subblock composition"));
}

#[test]
fn non_rgb8_subblocks_use_typed_level_composition_and_reject_local_rgb_path() {
    let mut bgr48 = SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 0, 2, 0, 3, 0]);
    bgr48.pixel_type = 4;
    let fixture = write_fixture(&[bgr48], &[], &metadata_xml(1, 1));
    let slide = ZeissSlide::parse(fixture.path()).expect("parse BGR48 CZI");
    let level = slide
        .scene_level_image(0, 0)
        .expect("compose typed BGR48 level");
    assert_eq!(level.data.as_u16(), Some([3, 2, 1].as_slice()));
    let error = slide
        .read_tile(0, 0, 0, 0, 0, BackendRequest::Cpu)
        .expect_err("local tile path requires u8 RGB");
    assert!(error.to_string().contains("8-bit RGB-compatible"));

    let mut gray = SubblockSpec::bgr24(0, 0, 1, 1, vec![9]);
    gray.pixel_type = 0;
    let fixture = write_fixture(&[gray], &[], &metadata_xml(1, 1));
    let slide = ZeissSlide::parse(fixture.path()).expect("parse Gray8 CZI");
    let error = slide
        .scene_level_image(0, 0)
        .expect_err("Gray8 cannot become RGB output");
    assert!(error.to_string().contains("unsupported Zeiss pixel type"));
}

#[test]
fn local_tile_geometry_rejects_origins_outside_the_i32_compositor_space() {
    let fixture = main_fixture();
    let mut slide = ZeissSlide::parse(fixture.path()).expect("parse generated CZI");
    let level = &mut slide.dataset.scenes[0].series[0].levels[0];
    level.dimensions = (u64::from(u32::MAX) + 1, 1);
    level.tile_layout = TileLayout::Regular {
        tile_width: u32::MAX,
        tile_height: 1,
        tiles_across: 2,
        tiles_down: 1,
    };
    slide.canvas_level_tile_subblocks[0].insert((1, 0), vec![0]);
    let error = slide
        .read_tile(0, 0, 0, 1, 0, BackendRequest::Cpu)
        .expect_err("oversized local x origin must fail");
    assert!(error.to_string().contains("tile x overflow"));

    let mut slide = ZeissSlide::parse(fixture.path()).expect("parse generated CZI again");
    let level = &mut slide.dataset.scenes[0].series[0].levels[0];
    level.dimensions = (1, u64::from(u32::MAX) + 1);
    level.tile_layout = TileLayout::Regular {
        tile_width: 1,
        tile_height: u32::MAX,
        tiles_across: 1,
        tiles_down: 2,
    };
    slide.canvas_level_tile_subblocks[0].insert((0, 1), vec![0]);
    let error = slide
        .read_tile(0, 0, 0, 0, 1, BackendRequest::Cpu)
        .expect_err("oversized local y origin must fail");
    assert!(error.to_string().contains("tile y overflow"));
}

#[test]
fn typed_level_composition_recovers_a_poisoned_czi_reader_mutex() {
    let mut bgr48 = SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 0, 2, 0, 3, 0]);
    bgr48.pixel_type = 4;
    let fixture = write_fixture(&[bgr48], &[], &metadata_xml(1, 1));
    let slide = ZeissSlide::parse(fixture.path()).expect("parse BGR48 CZI");

    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = slide.czi.lock().expect("unpoisoned CZI reader");
        panic!("poison synthetic CZI reader mutex");
    }));
    assert!(slide.czi.is_poisoned());

    let level = slide
        .scene_level_image(0, 0)
        .expect("recover poisoned CZI reader while composing");
    assert_eq!(level.data.as_u16(), Some([3, 2, 1].as_slice()));
}

#[test]
fn bitmap_blit_and_raw_conversion_reject_arithmetic_overflow() {
    let mut destination = Bitmap::zeros(PixelType::Bgr24, 1, 1).expect("destination bitmap");
    let mut source = Bitmap::zeros(PixelType::Bgr24, 1, 3).expect("source bitmap");
    source.stride = usize::MAX;
    let error =
        blit_tile(&mut destination, &source, 0, -2).expect_err("source stride overflow must fail");
    assert!(error.to_string().contains("source tile offset overflow"));

    let mut destination = Bitmap::zeros(PixelType::Bgr24, 1, 3).expect("tall destination");
    destination.stride = usize::MAX;
    let source = Bitmap::zeros(PixelType::Bgr24, 1, 1).expect("one-pixel source");
    let error = blit_tile(&mut destination, &source, 0, 2)
        .expect_err("destination stride overflow must fail");
    assert!(error
        .to_string()
        .contains("destination tile offset overflow"));

    let oversized = raw(
        PixelType::Bgra32,
        CompressionMode::UnCompressed,
        (u32::MAX, u32::MAX),
        Vec::new(),
    );
    let error = bitmap_from_raw_uncompressed_subblock(&oversized)
        .expect_err("oversized raw bitmap must fail before allocation");
    assert!(error.to_string().contains("bitmap size overflow"));
}

#[test]
fn typed_level_composition_rejects_oversized_destination_before_allocation() {
    let mut bgr48 = SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 0, 2, 0, 3, 0]);
    bgr48.pixel_type = 4;
    let fixture = write_fixture(&[bgr48], &[], &metadata_xml(1, 1));
    let mut slide = ZeissSlide::parse(fixture.path()).expect("parse BGR48 CZI");
    slide.dataset.scenes[0].series[0].levels[0].dimensions =
        (u64::from(u32::MAX), u64::from(u32::MAX));

    let error = slide
        .scene_level_image(0, 0)
        .expect_err("oversized typed composition must fail before allocation");
    assert!(error.to_string().contains("bitmap allocation"));
}

#[cfg(unix)]
#[test]
fn local_and_level_reads_report_errors_from_the_open_czi_inode() {
    fn assert_read_error<T: std::fmt::Debug>(result: Result<T, crate::WsiError>, context: &str) {
        let error = result.expect_err(context);
        assert!(
            error.to_string().contains("I/O") || error.to_string().contains("failed"),
            "unexpected CZI read error: {error}"
        );
    }

    let fixture = main_fixture();
    let slide = ZeissSlide::parse(fixture.path()).expect("parse direct local CZI");
    let open_inode = replace_path_and_truncate_open_fixture(&fixture);
    let result = slide.read_tile(0, 0, 0, 0, 0, BackendRequest::Cpu);
    std::fs::remove_file(open_inode).expect("remove detached direct local inode");
    assert_read_error(result, "truncated direct local reader must fail");

    let mut bgr48 = SubblockSpec::bgr24(0, 0, 1, 1, vec![1, 0, 2, 0, 3, 0]);
    bgr48.pixel_type = 4;
    let fixture = write_fixture(&[bgr48.clone()], &[], &metadata_xml(1, 1));
    let slide = ZeissSlide::parse(fixture.path()).expect("parse typed local CZI");
    let open_inode = replace_path_and_truncate_open_fixture(&fixture);
    let result = slide.read_tile(0, 0, 0, 0, 0, BackendRequest::Cpu);
    std::fs::remove_file(open_inode).expect("remove detached typed local inode");
    assert_read_error(result, "truncated typed local reader must fail");

    let fixture = main_fixture();
    let slide = ZeissSlide::parse(fixture.path()).expect("parse direct level CZI");
    let open_inode = replace_path_and_truncate_open_fixture(&fixture);
    let result = slide.scene_level_image(0, 0);
    std::fs::remove_file(open_inode).expect("remove detached direct level inode");
    assert_read_error(result, "truncated direct level reader must fail");

    let fixture = write_fixture(&[bgr48], &[], &metadata_xml(1, 1));
    let slide = ZeissSlide::parse(fixture.path()).expect("parse typed level CZI");
    let open_inode = replace_path_and_truncate_open_fixture(&fixture);
    let result = slide.scene_level_image(0, 0);
    std::fs::remove_file(open_inode).expect("remove detached typed level inode");
    assert_read_error(result, "truncated typed level reader must fail");
}
