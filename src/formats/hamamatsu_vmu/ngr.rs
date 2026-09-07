//! Little-endian NGR columns; layout documented by OpenSlide's ImHex pattern.
use std::fs::File;
use std::path::{Path, PathBuf};

use super::invalid;
use crate::core::positioned_file::PositionedFile;
use crate::core::types::*;
use crate::error::WsiError;

pub(super) struct Ngr {
    file: PositionedFile,
    path: PathBuf,
    pub width: u32,
    pub height: u32,
    column_width: u32,
    start: u64,
}

impl Ngr {
    pub(super) fn open(path: &Path) -> Result<Self, WsiError> {
        let file = File::open(path).map_err(|e| io_error(path, e))?;
        let length = file.metadata().map_err(|e| io_error(path, e))?.len();
        let file = PositionedFile::new(file);
        let mut header = [0u8; 28];
        file.read_exact_at(&mut header, 0)
            .map_err(|e| io_error(path, e))?;
        if &header[..2] != b"GN" {
            return Err(invalid(path, "NGR magic must be GN"));
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
            return Err(invalid(
                path,
                "invalid NGR dimensions, column width, or data offset",
            ));
        }
        let end = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(6))
            .and_then(|n| n.checked_add(start))
            .ok_or_else(|| invalid(path, "NGR data span overflow"))?;
        if end > length {
            return Err(invalid(path, "truncated NGR pixel data"));
        }
        Ok(Self {
            file,
            path: path.to_path_buf(),
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
            return Err(WsiError::TileRead {
                col: req.col,
                row: req.row,
                level: req.level.get(),
                reason: "NGR tile coordinates out of range".into(),
            });
        }
        let x = req.col as u32 * tw;
        let y = req.row as u32 * th;
        Ok((x, y, tw.min(self.width - x), th.min(self.height - y)))
    }

    pub(super) fn read_tile(&self, req: &TileRequest, limit: u64) -> Result<CpuTile, WsiError> {
        let (x, y, width, height) = self.tile_dimensions(req)?;
        let bytes = u64::from(width) * u64::from(height) * 6;
        if bytes > limit {
            return Err(WsiError::ResourceLimit {
                resource: "encoded tile/frame unit",
                requested: bytes,
                limit,
            });
        }
        // Maximum 256 × 64 RGB16 samples, independent of whole-slide size.
        let mut samples = vec![0u16; width as usize * height as usize * 3];
        let mut row = vec![0u8; width as usize * 6];
        for dy in 0..height {
            let mut dx = 0;
            while dx < width {
                let sx = x + dx;
                let column = sx / self.column_width;
                let within = sx % self.column_width;
                let count = (self.column_width - within).min(width - dx);
                // Open validates the complete span, so all in-bounds offsets fit u64.
                let pixel =
                    u64::from(column) * u64::from(self.height) * u64::from(self.column_width)
                        + u64::from(y + dy) * u64::from(self.column_width)
                        + u64::from(within);
                let data = &mut row[..count as usize * 6];
                self.file
                    .read_exact_at(data, self.start + pixel * 6)
                    .map_err(|e| io_error(&self.path, e))?;
                let dest = (dy as usize * width as usize + dx as usize) * 3;
                for (out, pair) in samples[dest..dest + count as usize * 3]
                    .iter_mut()
                    .zip(data.chunks_exact(2))
                {
                    *out = u16::from_le_bytes([pair[0], pair[1]]);
                }
                dx += count;
            }
        }
        CpuTile::new(
            width,
            height,
            3,
            ColorSpace::Rgb,
            CpuTileLayout::Interleaved,
            CpuTileData::u16(samples),
        )
    }
}

fn io_error(path: &Path, source: std::io::Error) -> WsiError {
    WsiError::IoWithPath {
        source: std::sync::Arc::new(source),
        path: path.to_path_buf(),
    }
}
