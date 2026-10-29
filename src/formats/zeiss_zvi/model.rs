use super::*;

pub(super) struct ZviSlide {
    pub(super) dataset: Dataset,
    pub(super) compound: Mutex<CompoundFile<File>>,
    /// The compound file as opened, shared by its tile locations.
    pub(super) source_path: Arc<Path>,
    /// The opened file, so locations map the same file's sector chains.
    pub(super) source_identity: FileIdentity,
    /// Bound on the payload ranges kept for tile locations.
    pub(super) location_index_limit: u64,
    /// Working set of a raw read that crops a JPEG plane: the lossless-crop
    /// limit, which the conservative wrapper this reader opens under
    /// reserves for such reads.
    pub(super) lossless_crop_bytes: u64,
    /// The slide's encoded-unit limit, which a crop's output must fit.
    pub(super) encoded_unit_bytes: u64,
    /// File ranges of each plane's payload, in plane order, mapped from the
    /// compound file's tables on the first location request.
    pub(super) payload_segments: OnceLock<Vec<Vec<ByteRange>>>,
    pub(super) planes: Vec<ZviPlane>,
    pub(super) plane_by_whole: HashMap<(u32, u32, u32), usize>,
    pub(super) plane_by_tile: HashMap<(u32, u32, u32, i64, i64), usize>,
    pub(super) associated: HashMap<String, CpuTile>,
}

#[derive(Clone)]
pub(super) struct ZviPlane {
    pub(super) stream_path: String,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) bytes_per_sample: u32,
    pub(super) payload_offset: u64,
    pub(super) payload_length: u64,
    pub(super) compression: ZviCompression,
    pub(super) z: u32,
    pub(super) c: u32,
    pub(super) t: u32,
    pub(super) tile_index: i32,
    pub(super) stage_position: Option<(f64, f64)>,
    pub(super) pixel_offset: (i64, i64),
    pub(super) grid_key: Option<(i64, i64)>,
    pub(super) channel_name: Option<String>,
    pub(super) channel_color: Option<[u8; 3]>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum ZviCompression {
    Raw,
    Zlib,
    Jpeg,
}

pub(super) struct ZviImageHeader {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) bytes_per_sample: u32,
    pub(super) payload_offset: u64,
    pub(super) compression: ZviCompression,
    pub(super) z: u32,
    pub(super) c: u32,
    pub(super) t: u32,
    pub(super) tile_index: i32,
}

pub(super) struct MosaicGrid {
    pub(super) advance_x: f64,
    pub(super) advance_y: f64,
    pub(super) width: u64,
    pub(super) height: u64,
    pub(super) entries: HashMap<(i64, i64), TileEntry>,
}

#[derive(Clone, Copy)]
pub(super) struct RawReadWindow {
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) bytes_per_sample: u64,
}

/// The stored plane a tile reads.
pub(super) enum ZviTileSource {
    /// The plane at `index` and the window of it the tile covers.
    Plane { index: usize, window: PixelRect },
    /// No plane stores the tile, for the reason given.
    Missing(&'static str),
}
