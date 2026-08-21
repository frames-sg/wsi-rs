use std::fs;
use std::io::{Cursor, Write};
use std::path::PathBuf;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use image::{DynamicImage, ImageFormat, RgbImage};
use tempfile::TempDir;

#[derive(Clone)]
pub(super) enum PlaneEncoding {
    Raw(Vec<u8>),
    Zlib(Vec<u8>),
    Jpeg(Vec<u8>),
}

#[derive(Clone)]
pub(super) struct PlaneSpec {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) bytes_per_sample: u32,
    pub(super) z: u32,
    pub(super) c: u32,
    pub(super) t: u32,
    pub(super) tile_index: i32,
    pub(super) encoding: PlaneEncoding,
    pub(super) tags: Vec<(i32, String)>,
}

impl PlaneSpec {
    pub(super) fn raw_u8(width: u32, height: u32, samples: Vec<u8>) -> Self {
        Self {
            width,
            height,
            bytes_per_sample: 1,
            z: 0,
            c: 0,
            t: 0,
            tile_index: 0,
            encoding: PlaneEncoding::Raw(samples),
            tags: Vec::new(),
        }
    }
}

pub(super) struct ZviFixture {
    _temp: TempDir,
    pub(super) path: PathBuf,
}

impl ZviFixture {
    pub(super) fn whole_u8() -> Self {
        let width = 8;
        let height = 4;
        let raw = (0..width * height).map(|value| value as u8).collect();
        let zlib_samples = (0..width * height)
            .map(|value| 100u8.wrapping_add(value as u8))
            .collect::<Vec<_>>();
        let jpeg_samples = (0..width * height)
            .map(|value| 200u8.saturating_add((value % 20) as u8))
            .collect::<Vec<_>>();
        let planes = vec![
            PlaneSpec {
                tags: channel_tags("Raw", 0x11_22_33),
                ..PlaneSpec::raw_u8(width, height, raw)
            },
            PlaneSpec {
                c: 1,
                encoding: PlaneEncoding::Zlib(zlib_samples),
                tags: channel_tags("Inflated", 0x44_55_66),
                ..PlaneSpec::raw_u8(width, height, Vec::new())
            },
            PlaneSpec {
                c: 2,
                encoding: PlaneEncoding::Jpeg(encode_gray_jpeg(width, height, &jpeg_samples)),
                tags: channel_tags("JPEG", 0x77_88_99),
                ..PlaneSpec::raw_u8(width, height, Vec::new())
            },
        ];
        Self::write(
            &planes,
            &[
                (515, width.to_string()),
                (516, height.to_string()),
                (769, "0.500000".into()),
                (772, "0.750000".into()),
                (2049, "Plan-Apochromat 20x".into()),
                (2076, "20".into()),
            ],
            Some(&thumbnail_bmp()),
        )
    }

    pub(super) fn raw_u16() -> Self {
        let width = 260;
        let height = 2;
        let samples = (0..width * height)
            .map(|value| (value as u16).wrapping_mul(3))
            .collect::<Vec<_>>();
        let bytes = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        Self::write(
            &[PlaneSpec {
                width,
                height,
                bytes_per_sample: 2,
                z: 0,
                c: 0,
                t: 0,
                tile_index: 0,
                encoding: PlaneEncoding::Raw(bytes),
                tags: Vec::new(),
            }],
            &[(515, width.to_string()), (516, height.to_string())],
            None,
        )
    }

    pub(super) fn mosaic() -> Self {
        let width = 256;
        let height = 2;
        let first = vec![17; width as usize * height as usize];
        let second = vec![231; width as usize * height as usize];
        let planes = vec![
            PlaneSpec {
                tile_index: 1,
                tags: vec![(2073, "0".into()), (2074, "0".into())],
                ..PlaneSpec::raw_u8(width, height, first)
            },
            PlaneSpec {
                tile_index: 2,
                tags: vec![(2073, "256".into()), (2074, "0".into())],
                ..PlaneSpec::raw_u8(width, height, second)
            },
        ];
        Self::write(
            &planes,
            &[
                (515, "512".into()),
                (516, height.to_string()),
                (769, "1".into()),
                (772, "1".into()),
            ],
            None,
        )
    }

    pub(super) fn write(
        planes: &[PlaneSpec],
        global_tags: &[(i32, String)],
        thumbnail: Option<&[u8]>,
    ) -> Self {
        let temp = tempfile::tempdir().expect("temporary ZVI fixture directory");
        let path = temp.path().join("synthetic.zvi");
        let mut compound = cfb::create(&path).expect("create synthetic ZVI compound file");
        write_stream(
            &mut compound,
            "/Image/Tags/Contents",
            &tag_stream(global_tags),
        );
        for (index, plane) in planes.iter().enumerate() {
            let contents = format!("/Image/Item({index})/Contents");
            write_stream(&mut compound, &contents, &plane_stream(plane));
            if !plane.tags.is_empty() {
                let tags = format!("/Image/Item({index})/Tags/Contents");
                write_stream(&mut compound, &tags, &tag_stream(&plane.tags));
            }
        }
        if let Some(thumbnail) = thumbnail {
            let mut wrapped = b"thumbnail-prefix".to_vec();
            wrapped.extend_from_slice(thumbnail);
            write_stream(&mut compound, "/Thumbnail", &wrapped);
        }
        drop(compound);
        Self { _temp: temp, path }
    }

    pub(super) fn rewrite_stream(&self, path: &str, bytes: &[u8]) {
        let mut compound = cfb::open_rw(&self.path).expect("open synthetic ZVI for mutation");
        if compound.is_stream(path) {
            compound.remove_stream(path).expect("remove old ZVI stream");
        }
        write_stream(&mut compound, path, bytes);
    }

    pub(super) fn remove_stream(&self, path: &str) {
        cfb::open_rw(&self.path)
            .expect("open synthetic ZVI for stream removal")
            .remove_stream(path)
            .expect("remove synthetic ZVI stream");
    }
}

pub(super) fn empty_compound() -> ZviFixture {
    let temp = tempfile::tempdir().expect("temporary empty CFB directory");
    let path = temp.path().join("empty.zvi");
    drop(cfb::create(&path).expect("create empty compound file"));
    ZviFixture { _temp: temp, path }
}

pub(super) fn plane_stream(spec: &PlaneSpec) -> Vec<u8> {
    let mut bytes = header_bytes(
        spec,
        match spec.encoding {
            PlaneEncoding::Raw(_) => 2,
            PlaneEncoding::Zlib(_) | PlaneEncoding::Jpeg(_) => 1,
        },
    );
    match &spec.encoding {
        PlaneEncoding::Raw(payload) | PlaneEncoding::Jpeg(payload) => {
            bytes.extend_from_slice(payload);
        }
        PlaneEncoding::Zlib(payload) => {
            bytes.extend_from_slice(b"WZL\0");
            bytes.extend_from_slice(&[0; 4]);
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(payload)
                .expect("compress synthetic ZVI plane");
            bytes.extend_from_slice(&encoder.finish().expect("finish synthetic ZVI zlib"));
        }
    }
    bytes
}

pub(super) fn header_bytes(spec: &PlaneSpec, valid: i32) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..11 {
        push_empty_variant(&mut bytes);
    }
    bytes.extend_from_slice(&[0; 2]);
    bytes.extend_from_slice(&28_i32.to_le_bytes());
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&(spec.z as i32).to_le_bytes());
    bytes.extend_from_slice(&(spec.c as i32).to_le_bytes());
    bytes.extend_from_slice(&(spec.t as i32).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&spec.tile_index.to_le_bytes());
    for _ in 0..5 {
        push_empty_variant(&mut bytes);
    }
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(spec.width as i32).to_le_bytes());
    bytes.extend_from_slice(&(spec.height as i32).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(spec.bytes_per_sample as i32).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&valid.to_le_bytes());
    assert_eq!(bytes.len(), 94);
    bytes
}

pub(super) fn tag_stream(tags: &[(i32, String)]) -> Vec<u8> {
    let mut bytes = vec![0; 8];
    bytes.extend_from_slice(&(tags.len() as i32).to_le_bytes());
    for (tag_id, value) in tags {
        bytes.extend_from_slice(&66_u16.to_le_bytes());
        bytes.extend_from_slice(&(value.len() as u16).to_le_bytes());
        bytes.extend_from_slice(value.as_bytes());
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&tag_id.to_le_bytes());
        bytes.extend_from_slice(&[0; 6]);
    }
    bytes
}

pub(super) fn encode_gray_jpeg(width: u32, height: u32, samples: &[u8]) -> Vec<u8> {
    j2k_jpeg::encode_jpeg_baseline(
        j2k_jpeg::JpegSamples::Gray8 {
            data: samples,
            width,
            height,
        },
        j2k_jpeg::JpegEncodeOptions {
            quality: 95,
            subsampling: j2k_jpeg::JpegSubsampling::Gray,
            restart_interval: None,
            backend: j2k_jpeg::JpegBackend::Cpu,
        },
    )
    .expect("encode synthetic grayscale JPEG")
    .data
}

fn thumbnail_bmp() -> Vec<u8> {
    let image = RgbImage::from_raw(2, 1, vec![255, 0, 0, 0, 255, 0])
        .expect("construct synthetic thumbnail");
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image)
        .write_to(&mut output, ImageFormat::Bmp)
        .expect("encode synthetic thumbnail BMP");
    output.into_inner()
}

fn channel_tags(name: &str, color: u32) -> Vec<(i32, String)> {
    vec![(1284, name.into()), (1282, color.to_string())]
}

fn push_empty_variant(bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(&0_u16.to_le_bytes());
}

fn write_stream(compound: &mut cfb::CompoundFile<fs::File>, path: &str, bytes: &[u8]) {
    let parent = PathBuf::from(path)
        .parent()
        .expect("synthetic stream has parent")
        .to_path_buf();
    compound
        .create_storage_all(parent)
        .expect("create synthetic ZVI storage");
    compound
        .create_stream(path)
        .expect("create synthetic ZVI stream")
        .write_all(bytes)
        .expect("write synthetic ZVI stream");
}
