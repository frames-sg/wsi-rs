use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::super::{ETS_BACKGROUND_BYTES, OLYMPUS_JPEG_2000};

pub(super) const ADDITIONAL_HEADER_OFFSET: usize = 64;
pub(super) const CHUNK_TABLE_OFFSET: usize = 256;
pub(super) const RGB_CODESTREAM: &[u8] =
    include_bytes!("../../../../tests/fixtures/jp2k/rgb_nomct.j2k");

#[derive(Clone)]
pub(super) struct ChunkSpec {
    pub(super) coords: Vec<i32>,
    payload: Vec<u8>,
    pub(super) declared_offset: Option<u64>,
    pub(super) declared_len: Option<u32>,
}

impl ChunkSpec {
    pub(super) fn new(coords: &[i32], payload: &[u8]) -> Self {
        Self {
            coords: coords.to_vec(),
            payload: payload.to_vec(),
            declared_offset: None,
            declared_len: None,
        }
    }
}

#[derive(Clone)]
pub(super) struct EtsSpec {
    pub(super) n_dimensions: u32,
    pub(super) pixel_type: u32,
    pub(super) samples_per_pixel: u32,
    pub(super) compression: u32,
    pub(super) tile_width: u32,
    pub(super) tile_height: u32,
    pub(super) background: Vec<u8>,
    pub(super) use_pyramid: bool,
    pub(super) chunks: Vec<ChunkSpec>,
}

impl Default for EtsSpec {
    fn default() -> Self {
        Self {
            n_dimensions: 3,
            pixel_type: 1,
            samples_per_pixel: 3,
            compression: OLYMPUS_JPEG_2000,
            tile_width: 16,
            tile_height: 12,
            background: vec![7, 11, 13],
            use_pyramid: false,
            chunks: vec![ChunkSpec::new(&[1, 0, 0], RGB_CODESTREAM)],
        }
    }
}

pub(super) struct VsiFixture {
    _temp: TempDir,
    pub(super) path: PathBuf,
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(super) fn build_ets(spec: &EtsSpec) -> Vec<u8> {
    assert!(!spec.chunks.is_empty());
    assert!(spec.background.len() <= ETS_BACKGROUND_BYTES as usize);
    assert!(spec
        .chunks
        .iter()
        .all(|chunk| chunk.coords.len() == spec.n_dimensions as usize));

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"SIS\0");
    push_u32(&mut bytes, 48);
    push_u32(&mut bytes, 1);
    push_u32(&mut bytes, spec.n_dimensions);
    push_u64(&mut bytes, ADDITIONAL_HEADER_OFFSET as u64);
    push_u32(&mut bytes, 156);
    push_u32(&mut bytes, 0);
    push_u64(&mut bytes, CHUNK_TABLE_OFFSET as u64);
    push_u32(&mut bytes, spec.chunks.len() as u32);
    push_u32(&mut bytes, 0);

    bytes.resize(ADDITIONAL_HEADER_OFFSET, 0);
    bytes.extend_from_slice(b"ETS\0");
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, spec.pixel_type);
    push_u32(&mut bytes, spec.samples_per_pixel);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, spec.compression);
    push_u32(&mut bytes, 100);
    push_u32(&mut bytes, spec.tile_width);
    push_u32(&mut bytes, spec.tile_height);
    push_u32(&mut bytes, 1);
    for _ in 0..17 {
        push_u32(&mut bytes, 0);
    }
    bytes.extend_from_slice(&spec.background);
    bytes.resize(
        ADDITIONAL_HEADER_OFFSET + 108 + ETS_BACKGROUND_BYTES as usize,
        0,
    );
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, u32::from(spec.use_pyramid));

    bytes.resize(CHUNK_TABLE_OFFSET, 0);
    let entry_len = 20 + spec.n_dimensions as usize * 4;
    let payload_start = CHUNK_TABLE_OFFSET + entry_len * spec.chunks.len();
    let mut next_payload_offset = payload_start as u64;
    for chunk in &spec.chunks {
        push_u32(&mut bytes, 0);
        for coord in &chunk.coords {
            bytes.extend_from_slice(&coord.to_le_bytes());
        }
        push_u64(
            &mut bytes,
            chunk.declared_offset.unwrap_or(next_payload_offset),
        );
        push_u32(
            &mut bytes,
            chunk.declared_len.unwrap_or(chunk.payload.len() as u32),
        );
        push_u32(&mut bytes, 0);
        next_payload_offset += chunk.payload.len() as u64;
    }
    for chunk in &spec.chunks {
        bytes.extend_from_slice(&chunk.payload);
    }
    bytes
}

pub(super) fn write_vsi_fixture(scenes: &[(&str, EtsSpec)]) -> VsiFixture {
    let temp = tempfile::tempdir().expect("temporary VSI fixture directory");
    let path = temp.path().join("synthetic.vsi");
    drop(cfb::create(&path).expect("create minimal compound VSI file"));
    let companion = temp.path().join("_synthetic_");
    for (name, spec) in scenes {
        let scene_dir = companion.join(name);
        fs::create_dir_all(&scene_dir).expect("create ETS scene directory");
        fs::write(scene_dir.join("frame_t.ets"), build_ets(spec)).expect("write ETS fixture");
    }
    VsiFixture { _temp: temp, path }
}

pub(super) fn write_ets(bytes: &[u8]) -> (TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temporary ETS fixture directory");
    let path = temp.path().join("frame_t.ets");
    fs::write(&path, bytes).expect("write ETS fixture");
    (temp, path)
}
