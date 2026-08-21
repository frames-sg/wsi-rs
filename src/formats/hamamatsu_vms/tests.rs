use super::model::VmsJpeg;
use super::{jpeg::read_vms_jpeg_header, *};

mod backend;
mod errors;
pub(super) mod fixtures;

use fixtures::write_restart_jpeg;

#[test]
fn vms_jpeg_header_probe_reads_only_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tile.jpg");
    let width = 128u32;
    let height = 16u32;
    let mut bytes = write_restart_jpeg(&path, width, height);
    let encoded_len = bytes.len();
    bytes.extend(vec![0xA5; 1_000_000]);
    std::fs::write(&path, bytes).unwrap();

    let header = read_vms_jpeg_header(&path).unwrap();

    assert_eq!(header.geometry.width, width);
    assert_eq!(header.geometry.height, height);
    assert_eq!(header.geometry.tile_width, 64);
    assert_eq!(header.geometry.tile_height, 8);
    assert!(header.header.len() < encoded_len);
    assert!(header.header.len() < 4096);
}

#[test]
fn vms_jpeg_decodes_restart_segment_tile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tile.jpg");
    let encoded = write_restart_jpeg(&path, 128, 16);
    let reference = J2kJpegDecoder::new(&encoded)
        .unwrap()
        .decode_request(J2kJpegDecodeRequest::region_scaled(
            J2kPixelFormat::Rgb8,
            J2kRect {
                x: 64,
                y: 8,
                w: 64,
                h: 8,
            },
            J2kDownscale::None,
        ))
        .unwrap()
        .0;
    let restart_index = J2kJpegView::parse(&encoded)
        .unwrap()
        .restart_index()
        .unwrap()
        .unwrap();
    let row_starts = vec![
        Some(restart_index.segments[0].entropy_offset as u64),
        Some(restart_index.segments[2].entropy_offset as u64),
    ];
    let jpeg = VmsJpeg::parse(&path, row_starts).unwrap();

    let tile = jpeg.decode_tile(3, 1, BackendRequest::Auto).unwrap();

    assert_eq!(tile.width, 64);
    assert_eq!(tile.height, 8);
    assert_eq!(tile.data.as_u8().unwrap(), reference.as_slice());
    assert_eq!(
        jpeg.decoded_tile_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len(),
        1
    );
}

#[test]
fn vms_private_tile_cache_capacity_tracks_cache_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tile.jpg");
    write_restart_jpeg(&path, 128, 16);
    let small = VmsJpeg::parse_with_cache_config(
        &path,
        Vec::new(),
        CacheConfig::deterministic().with_shared_tile_bytes(12 * 1024),
    )
    .unwrap();
    let large = VmsJpeg::parse_with_cache_config(
        &path,
        Vec::new(),
        CacheConfig::deterministic().with_shared_tile_bytes(48 * 1024),
    )
    .unwrap();

    assert_eq!(
        small
            .decoded_tile_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capacity_entries(),
        2
    );
    assert_eq!(
        large
            .decoded_tile_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .capacity_entries(),
        8
    );
}
