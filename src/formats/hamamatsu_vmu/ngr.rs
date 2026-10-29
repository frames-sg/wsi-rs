//! Little-endian NGR columns; layout documented by OpenSlide's ImHex pattern.
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::positioned_file::PositionedFile;
use crate::core::types::*;
use crate::error::WsiError;

/// Bytes of one stored pixel: three little-endian 16-bit samples.
const PIXEL_BYTES: u64 = 6;

pub(super) struct Ngr {
    file: PositionedFile,
    path: PathBuf,
    /// `path`, shared by every tile location.
    location_path: Arc<Path>,
    file_len: u64,
    pub width: u32,
    pub height: u32,
    column_width: u32,
    start: u64,
}

impl Ngr {
    pub(super) fn open(path: &Path) -> Result<Self, WsiError> {
        let file = File::open(path).map_err(|e| WsiError::io_with_path(path, e))?;
        let length = file
            .metadata()
            .map_err(|e| WsiError::io_with_path(path, e))?
            .len();
        let file = PositionedFile::new(file);
        let mut header = [0u8; 28];
        file.read_exact_at(&mut header, 0)
            .map_err(|e| WsiError::io_with_path(path, e))?;
        if &header[..2] != b"GN" {
            return Err(WsiError::invalid_slide(path, "NGR magic must be GN"));
        }
        let word = |offset| {
            u32::from_le_bytes(
                header[offset..offset + 4]
                    .try_into()
                    .expect("fixed header word"),
            )
        };
        let (width, height, column_width, start) =
            (word(4), word(8), word(12), u64::from(word(24)));
        if width == 0 || height == 0 || column_width == 0 || width % column_width != 0 || start < 28
        {
            return Err(WsiError::invalid_slide(
                path,
                "invalid NGR dimensions, column width, or data offset",
            ));
        }
        let end = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(6))
            .and_then(|n| n.checked_add(start))
            .ok_or_else(|| WsiError::invalid_slide(path, "NGR data span overflow"))?;
        if end > length {
            return Err(WsiError::invalid_slide(path, "truncated NGR pixel data"));
        }
        Ok(Self {
            file,
            path: path.to_path_buf(),
            location_path: Arc::from(path),
            file_len: length,
            width,
            height,
            column_width,
            start,
        })
    }

    // Virtual tiles cap allocations independently of a scanner's column width.
    pub(super) fn level(&self, downsample: f64) -> Level {
        let tile_width = self.column_width.min(256);
        let tile_height = self.height.min(64);
        Level {
            dimensions: (u64::from(self.width), u64::from(self.height)),
            downsample,
            tile_layout: TileLayout::Regular {
                tile_width,
                tile_height,
                tiles_across: u64::from(self.width.div_ceil(tile_width)),
                tiles_down: u64::from(self.height.div_ceil(tile_height)),
            },
        }
    }

    pub(super) fn tile_dimensions(
        &self,
        req: &TileRequest,
    ) -> Result<(u32, u32, u32, u32), WsiError> {
        let tw = self.column_width.min(256);
        let th = self.height.min(64);
        if req.col < 0
            || req.row < 0
            || req.col >= i64::from(self.width.div_ceil(tw))
            || req.row >= i64::from(self.height.div_ceil(th))
        {
            return Err(WsiError::tile_read(
                req,
                "NGR tile coordinates out of range",
            ));
        }
        let x = req.col as u32 * tw;
        let y = req.row as u32 * th;
        Ok((x, y, tw.min(self.width - x), th.min(self.height - y)))
    }

    pub(super) fn read_tile(&self, req: &TileRequest, limit: u64) -> Result<CpuTile, WsiError> {
        let (_, _, width, height) = self.tile_dimensions(req)?;
        let bytes = self.read_tile_bytes(req, limit)?;
        let samples = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        CpuTile::new(
            width,
            height,
            3,
            ColorSpace::Rgb,
            CpuTileLayout::Interleaved,
            CpuTileData::u16(samples),
        )
    }

    /// The tile's interleaved RGB16 samples in native byte order. They are
    /// the stored bytes, reordered only on big-endian hosts.
    pub(super) fn read_raw_tile(
        &self,
        req: &TileRequest,
        limit: u64,
    ) -> Result<RawCompressedTile, WsiError> {
        let (_, _, width, height) = self.tile_dimensions(req)?;
        let mut bytes = self.read_tile_bytes(req, limit)?;
        if cfg!(target_endian = "big") {
            for pair in bytes.as_chunks_mut::<2>().0 {
                pair.swap(0, 1);
            }
        }
        Ok(RawCompressedTile::builder(Compression::None)
            .dimensions(width, height)
            .bits_allocated(16)
            .samples_per_pixel(3)
            .photometric_interpretation(EncodedTilePhotometricInterpretation::Rgb)
            .data(bytes)
            .build()?)
    }

    /// Where the tile's samples are stored. Each NGR column the tile
    /// crosses is one bare little-endian RGB16 unit (12 significant bits),
    /// `count` pixels wide:
    /// one segment per tile row, or a single segment when the tile spans
    /// the whole column width and its rows are therefore contiguous.
    pub(super) fn tile_location(&self, req: &TileRequest) -> Result<TileLocation, WsiError> {
        let (x, y, width, height) = self.tile_dimensions(req)?;
        let codec = EncodedCodec::new(
            Compression::None,
            16,
            3,
            EncodedTilePhotometricInterpretation::Rgb,
        )
        .with_significant_bits(12)
        .with_byte_order(ByteOrder::LittleEndian);
        let mut units = Vec::new();
        for span in self.column_spans(x, width) {
            let rows: Vec<_> = if span.count == self.column_width {
                vec![self.pixel_range(span.column, y, 0, span.count * height)]
            } else {
                (0..height)
                    .map(|dy| self.pixel_range(span.column, y + dy, span.within, span.count))
                    .collect()
            };
            units.push(
                EncodedUnit::builder(
                    Arc::clone(&self.location_path),
                    codec.clone(),
                    span.count,
                    height,
                )
                .segments(rows)
                .framing(StoredFraming::BarePayload)
                .tile_region(PixelRect::new(span.dx, 0, span.count, height))
                .build(self.file_len)?,
            );
        }
        Ok(TileLocation::Stored(StoredTile::new(width, height, units)?))
    }

    /// Interleaved little-endian RGB16 bytes of the tile, as stored.
    fn read_tile_bytes(&self, req: &TileRequest, limit: u64) -> Result<Vec<u8>, WsiError> {
        let (x, y, width, height) = self.tile_dimensions(req)?;
        let bytes = u64::from(width) * u64::from(height) * PIXEL_BYTES;
        if bytes > limit {
            return Err(WsiError::ResourceLimit {
                resource: "encoded tile/frame unit",
                requested: bytes,
                limit,
            });
        }
        // Maximum 256 × 64 RGB16 pixels, independent of whole-slide size.
        let row_bytes = width as usize * PIXEL_BYTES as usize;
        let mut data = vec![0u8; row_bytes * height as usize];
        for span in self.column_spans(x, width) {
            for dy in 0..height {
                let range = self.pixel_range(span.column, y + dy, span.within, span.count);
                let dest = dy as usize * row_bytes + span.dx as usize * PIXEL_BYTES as usize;
                self.file
                    .read_exact_at(&mut data[dest..dest + range.length as usize], range.offset)
                    .map_err(|e| WsiError::io_with_path(&self.path, e))?;
            }
        }
        Ok(data)
    }

    /// The parts of image columns `x .. x + width` that lie in each NGR
    /// column, left to right.
    fn column_spans(&self, x: u32, width: u32) -> impl Iterator<Item = ColumnSpan> + '_ {
        let mut dx = 0;
        std::iter::from_fn(move || {
            (dx < width).then(|| {
                let sx = x + dx;
                let within = sx % self.column_width;
                let span = ColumnSpan {
                    column: sx / self.column_width,
                    within,
                    dx,
                    count: (self.column_width - within).min(width - dx),
                };
                dx += span.count;
                span
            })
        })
    }

    /// Bytes of `count` pixels starting at column-local `within` on image
    /// row `row` of NGR column `column`.
    fn pixel_range(&self, column: u32, row: u32, within: u32, count: u32) -> ByteRange {
        // Open validates the complete span, so all in-bounds offsets fit u64.
        let pixel = u64::from(column) * u64::from(self.height) * u64::from(self.column_width)
            + u64::from(row) * u64::from(self.column_width)
            + u64::from(within);
        ByteRange::new(
            self.start + pixel * PIXEL_BYTES,
            u64::from(count) * PIXEL_BYTES,
        )
    }
}

/// Part of a tile that lies in one NGR column.
struct ColumnSpan {
    column: u32,
    /// First pixel inside the column.
    within: u32,
    /// First pixel inside the tile.
    dx: u32,
    count: u32,
}
