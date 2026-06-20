use super::model::VmsJpeg;
use super::{jpeg::read_vms_jpeg_header, *};

fn write_restart_jpeg(path: &Path, width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; width as usize * height as usize * 3];
    for y in 0..height {
        for x in 0..width {
            let off = (y as usize * width as usize + x as usize) * 3;
            pixels[off] = x as u8;
            pixels[off + 1] = y as u8;
            pixels[off + 2] = x.wrapping_add(y) as u8;
        }
    }
    let encoded = j2k_jpeg::encode_jpeg_baseline(
        j2k_jpeg::JpegSamples::Rgb8 {
            data: &pixels,
            width,
            height,
        },
        j2k_jpeg::JpegEncodeOptions {
            quality: 90,
            subsampling: j2k_jpeg::JpegSubsampling::Ybr444,
            restart_interval: Some(8),
            backend: j2k_jpeg::JpegBackend::Cpu,
        },
    )
    .unwrap();
    std::fs::write(path, &encoded.data).unwrap();
    encoded.data
}

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
        .decode_region_scaled(
            J2kPixelFormat::Rgb8,
            J2kRect {
                x: 64,
                y: 8,
                w: 64,
                h: 8,
            },
            J2kDownscale::None,
        )
        .unwrap()
        .0;
    let restart_index = J2kJpegDecoder::new(&encoded)
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
