use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use super::super::error::{IfdId, TiffParseError};

// ── Endianness ─────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Endian {
    Little,
    Big,
}

// ── TIFF tag constants ─────────────────────────────────────────────

pub(crate) mod tags {
    pub const IMAGE_WIDTH: u16 = 256;
    pub const IMAGE_LENGTH: u16 = 257;
    pub const BITS_PER_SAMPLE: u16 = 258;
    pub const COMPRESSION: u16 = 259;
    pub const PHOTOMETRIC: u16 = 262;
    pub const IMAGE_DESCRIPTION: u16 = 270;
    pub const STRIP_OFFSETS: u16 = 273;
    pub const SAMPLES_PER_PIXEL: u16 = 277;
    pub const ROWS_PER_STRIP: u16 = 278;
    pub const STRIP_BYTE_COUNTS: u16 = 279;
    #[cfg(test)]
    pub const SUB_IFDS: u16 = 330;
    pub const TILE_WIDTH: u16 = 322;
    pub const TILE_LENGTH: u16 = 323;
    pub const TILE_OFFSETS: u16 = 324;
    pub const TILE_BYTE_COUNTS: u16 = 325;
    pub const X_RESOLUTION: u16 = 282;
    pub const Y_RESOLUTION: u16 = 283;
    pub const RESOLUTION_UNIT: u16 = 296;
    pub const PREDICTOR: u16 = 317;
    pub const JPEG_TABLES: u16 = 347;
    pub const XMP: u16 = 700;
    pub const ICC_PROFILE: u16 = 34675;
    /// Hamamatsu NDPI marker tag.
    pub const NDPI_MARKER: u16 = 65420;
}

// ── TiffType ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub(crate) enum TiffType {
    Byte,      // 1: u8
    Ascii,     // 2: null-terminated string
    Short,     // 3: u16
    Long,      // 4: u32
    Rational,  // 5: u32/u32
    SByte,     // 6: i8
    Undefined, // 7: arbitrary bytes
    SShort,    // 8: i16
    SLong,     // 9: i32
    SRational, // 10: i32/i32
    Float,     // 11: f32
    Double,    // 12: f64
    Ifd,       // 13: u32 IFD offset
    Long8,     // 16: u64 (BigTIFF)
    SLong8,    // 17: i64 (BigTIFF)
    Ifd8,      // 18: u64 IFD offset (BigTIFF)
}

impl TiffType {
    pub fn from_u16(id: u16) -> Option<TiffType> {
        match id {
            1 => Some(TiffType::Byte),
            2 => Some(TiffType::Ascii),
            3 => Some(TiffType::Short),
            4 => Some(TiffType::Long),
            5 => Some(TiffType::Rational),
            6 => Some(TiffType::SByte),
            7 => Some(TiffType::Undefined),
            8 => Some(TiffType::SShort),
            9 => Some(TiffType::SLong),
            10 => Some(TiffType::SRational),
            11 => Some(TiffType::Float),
            12 => Some(TiffType::Double),
            13 => Some(TiffType::Ifd),
            16 => Some(TiffType::Long8),
            17 => Some(TiffType::SLong8),
            18 => Some(TiffType::Ifd8),
            _ => None,
        }
    }

    pub fn byte_size(&self) -> u64 {
        match self {
            TiffType::Byte | TiffType::Ascii | TiffType::SByte | TiffType::Undefined => 1,
            TiffType::Short | TiffType::SShort => 2,
            TiffType::Long | TiffType::SLong | TiffType::Float | TiffType::Ifd => 4,
            TiffType::Rational
            | TiffType::SRational
            | TiffType::Double
            | TiffType::Long8
            | TiffType::SLong8
            | TiffType::Ifd8 => 8,
        }
    }
}

// ── InlineValue ────────────────────────────────────────────────────

/// Stores inline tag data. Invariant: len <= 12.
/// Capacity of 12 covers BigTIFF's 8-byte value slot with headroom.
#[derive(Clone, Debug)]
pub(crate) struct InlineValue {
    len: u8,
    bytes: [u8; 12],
}

impl InlineValue {
    pub fn new(data: &[u8]) -> Self {
        assert!(data.len() <= 12, "InlineValue data exceeds 12 bytes");
        let mut bytes = [0u8; 12];
        bytes[..data.len()].copy_from_slice(data);
        InlineValue {
            len: data.len() as u8,
            bytes,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

// ── TagValue ───────────────────────────────────────────────────────

pub(crate) enum TagValue {
    Inline(InlineValue),
    Lazy {
        offset: u64,
        byte_len: u64,
        resolved: OnceLock<Result<Vec<u8>, TiffParseError>>,
    },
}

impl std::fmt::Debug for TagValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TagValue::Inline(v) => f.debug_tuple("Inline").field(v).finish(),
            TagValue::Lazy {
                offset, byte_len, ..
            } => f
                .debug_struct("Lazy")
                .field("offset", offset)
                .field("byte_len", byte_len)
                .finish(),
        }
    }
}

// ── TagEntry ───────────────────────────────────────────────────────

pub(crate) struct TagEntry {
    pub tiff_type: TiffType,
    pub count: u64,
    pub value: TagValue,
    pub(super) decoded_u64: OnceLock<Vec<u64>>,
}

impl std::fmt::Debug for TagEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TagEntry")
            .field("tiff_type", &self.tiff_type)
            .field("count", &self.count)
            .field("value", &self.value)
            .finish()
    }
}

impl TagEntry {
    pub fn new_inline(tiff_type: TiffType, count: u64, data: &[u8]) -> Self {
        TagEntry {
            tiff_type,
            count,
            value: TagValue::Inline(InlineValue::new(data)),
            decoded_u64: OnceLock::new(),
        }
    }

    pub fn new_lazy(tiff_type: TiffType, count: u64, offset: u64, byte_len: u64) -> Self {
        TagEntry {
            tiff_type,
            count,
            value: TagValue::Lazy {
                offset,
                byte_len,
                resolved: OnceLock::new(),
            },
            decoded_u64: OnceLock::new(),
        }
    }
}

// ── Ifd ────────────────────────────────────────────────────────────

pub(crate) struct Ifd {
    pub id: IfdId,
    pub offset: u64,
    pub tags: HashMap<u16, TagEntry>,
    /// References to SubIFDs by stable ID.
    pub sub_ifds: Vec<IfdId>,
}

impl std::fmt::Debug for Ifd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ifd")
            .field("id", &self.id)
            .field("offset", &self.offset)
            .field("tag_count", &self.tags.len())
            .field("sub_ifds", &self.sub_ifds)
            .finish()
    }
}

// ── TiffContainer ──────────────────────────────────────────────────

pub(crate) struct TiffContainer {
    pub(super) path: Arc<PathBuf>,
    pub(super) file_len: u64,
    /// Persistent file handle for pread — avoids re-opening per call.
    /// pread(2) on Unix is atomic and doesn't use the file position, so a
    /// single File can be shared across threads safely.
    pub(super) file: std::fs::File,
    /// On Windows, `seek_read` is not atomic (it modifies the shared file
    /// position), so concurrent pread calls must be serialized.
    #[cfg(windows)]
    pub(super) pread_lock: std::sync::Mutex<()>,
    pub(super) endian: Endian,
    pub(super) bigtiff: bool,
    pub(super) ndpi: bool,
    /// Top-level IFD order as encountered in the main chain.
    pub(super) top_ifds: Vec<IfdId>,
    /// Flat arena: every parsed IFD lives here exactly once, keyed by byte offset.
    pub(super) ifds: HashMap<IfdId, Ifd>,
}

impl std::fmt::Debug for TiffContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TiffContainer")
            .field("path", &self.path)
            .field("endian", &self.endian)
            .field("bigtiff", &self.bigtiff)
            .field("ndpi", &self.ndpi)
            .field("ifd_count", &self.ifds.len())
            .finish()
    }
}
