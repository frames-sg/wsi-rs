use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

pub(super) struct VmsFixture {
    _temp: TempDir,
    pub(super) path: PathBuf,
    pub(super) image_paths: Vec<PathBuf>,
    pub(super) map_path: PathBuf,
    pub(super) macro_path: PathBuf,
    pub(super) opt_path: PathBuf,
}

fn patterned_pixels(width: u32, height: u32, bias: u8) -> Vec<u8> {
    let mut pixels = vec![0u8; width as usize * height as usize * 3];
    for y in 0..height {
        for x in 0..width {
            let off = (y as usize * width as usize + x as usize) * 3;
            pixels[off] = bias.wrapping_add(x as u8);
            pixels[off + 1] = bias.wrapping_add(y as u8);
            pixels[off + 2] = bias.wrapping_add(x.wrapping_add(y) as u8);
        }
    }
    pixels
}

fn encode_jpeg(width: u32, height: u32, bias: u8, restart_interval: Option<u16>) -> Vec<u8> {
    j2k_jpeg::encode_jpeg_baseline(
        j2k_jpeg::JpegSamples::Rgb8 {
            data: &patterned_pixels(width, height, bias),
            width,
            height,
        },
        j2k_jpeg::JpegEncodeOptions {
            quality: 90,
            subsampling: j2k_jpeg::JpegSubsampling::Ybr444,
            restart_interval,
            backend: j2k_jpeg::JpegBackend::Cpu,
        },
    )
    .expect("encode synthetic JPEG")
    .data
}

fn insert_comment(jpeg: &mut Vec<u8>, comment: &[u8]) {
    let segment_len = u16::try_from(comment.len() + 2).expect("small synthetic JPEG comment");
    let mut segment = Vec::with_capacity(comment.len() + 4);
    segment.extend_from_slice(&[0xFF, 0xFE]);
    segment.extend_from_slice(&segment_len.to_be_bytes());
    segment.extend_from_slice(comment);
    jpeg.splice(2..2, segment);
}

pub(in crate::formats::hamamatsu_vms) fn write_restart_jpeg(
    path: &Path,
    width: u32,
    height: u32,
) -> Vec<u8> {
    write_restart_jpeg_with_bias(path, width, height, 0, None).0
}

fn write_restart_jpeg_with_bias(
    path: &Path,
    width: u32,
    height: u32,
    bias: u8,
    comment: Option<&[u8]>,
) -> (Vec<u8>, Vec<u64>) {
    let mut data = encode_jpeg(width, height, bias, Some(8));
    if let Some(comment) = comment {
        insert_comment(&mut data, comment);
    }
    fs::write(path, &data).expect("write synthetic restart JPEG");

    let restart_index = j2k_jpeg::JpegView::parse(&data)
        .expect("parse synthetic restart JPEG")
        .restart_index()
        .expect("inspect restart index")
        .expect("restart index present");
    let tiles_across = width.div_ceil(64) as usize;
    let tiles_down = height.div_ceil(8) as usize;
    let row_starts = (0..tiles_down)
        .map(|row| restart_index.segments[row * tiles_across].entropy_offset as u64)
        .collect();
    (data, row_starts)
}

impl VmsFixture {
    pub(super) fn complete() -> Self {
        let temp = tempfile::tempdir().expect("temporary VMS fixture directory");
        let path = temp.path().join("synthetic.vms");
        let image_paths = vec![
            temp.path().join("image0.jpg"),
            temp.path().join("image1.jpg"),
        ];
        let map_path = temp.path().join("map.jpg");
        let macro_path = temp.path().join("macro.jpg");
        let opt_path = temp.path().join("optimisation.bin");

        let (_, first_offsets) = write_restart_jpeg_with_bias(
            &image_paths[0],
            128,
            16,
            5,
            Some(b"synthetic VMS comment\0ignored"),
        );
        let (_, second_offsets) = write_restart_jpeg_with_bias(&image_paths[1], 128, 16, 90, None);
        write_restart_jpeg_with_bias(&map_path, 64, 8, 170, None);
        fs::write(&macro_path, encode_jpeg(24, 16, 210, None)).expect("write synthetic macro JPEG");

        let mut opt = Vec::new();
        for offset in first_offsets.into_iter().chain(second_offsets) {
            opt.extend_from_slice(&offset.to_le_bytes());
            opt.resize(opt.len() + 32, 0);
        }
        fs::write(&opt_path, opt).expect("write VMS optimisation offsets");

        let fixture = Self {
            _temp: temp,
            path,
            image_paths,
            map_path,
            macro_path,
            opt_path,
        };
        fixture.write_key(&fixture.complete_key());
        fixture
    }

    pub(super) fn complete_key(&self) -> String {
        "[Virtual Microscope Specimen]\n\
         NoJpegColumns=2\n\
         NoJpegRows=1\n\
         ImageFile(0,0)=image0.jpg\n\
         ImageFile(1,0)=image1.jpg\n\
         ImageFile(1,0,0)=ignored-layer.jpg\n\
         MapFile=map.jpg\n\
         MacroImage=macro.jpg\n\
         OptimisationFile=optimisation.bin\n\
         SourceLens=40\n\
         PhysicalWidth=256000\n\
         PhysicalHeight=16000\n\
         Reference=synthetic\n"
            .into()
    }

    pub(super) fn write_key(&self, text: &str) {
        fs::write(&self.path, text).expect("write synthetic VMS key file");
    }
}

pub(in crate::formats::hamamatsu_vms) fn write_jpeg_header(path: &Path, segments: &[(u8, &[u8])]) {
    let mut data = vec![0xFF, 0xD8];
    for (marker, payload) in segments {
        data.extend_from_slice(&[0xFF, *marker]);
        let len = u16::try_from(payload.len() + 2).expect("small synthetic JPEG segment");
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(payload);
    }
    fs::write(path, data).expect("write synthetic JPEG header");
}
