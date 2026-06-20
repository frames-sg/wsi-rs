use std::path::Path;
#[cfg(windows)]
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

use super::super::error::{IfdId, TiffParseError};
use super::model::{Endian, Ifd, TagValue, TiffContainer, TiffType};
use super::ndpi_offsets::{fix_offset_ndpi, is_ndpi_data_offset_tag};

impl TiffContainer {
    // ── pread (stateless positional read) ──────────────────────

    /// Perform a positional read using the persistent file handle.
    /// Uses pread(2) on Unix for lock-free concurrent reads (no seek state).
    pub fn pread(&self, offset: u64, len: u64) -> Result<Vec<u8>, TiffParseError> {
        // Bounds check with checked arithmetic
        let end = offset
            .checked_add(len)
            .ok_or(TiffParseError::Bounds { offset, len })?;
        if end > self.file_len {
            return Err(TiffParseError::Bounds { offset, len });
        }

        let len_usize = len as usize;
        let mut buf = vec![0u8; len_usize];

        #[cfg(unix)]
        {
            self.file.read_exact_at(&mut buf, offset)?;
        }

        #[cfg(windows)]
        {
            // seek_read on Windows is not atomic (it modifies the shared file
            // position), so we must serialize concurrent pread calls.
            let _guard = self.pread_lock.lock().unwrap_or_else(|e| e.into_inner());
            let n = self.file.seek_read(&mut buf, offset)?;
            if n != len_usize {
                return Err(TiffParseError::Io {
                    kind: std::io::ErrorKind::UnexpectedEof,
                    source: Arc::new(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("seek_read: expected {} bytes, got {}", len_usize, n),
                    )),
                    path: Some(self.path.clone()),
                });
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(self.path.as_ref())?;
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut buf)?;
        }

        Ok(buf)
    }

    // ── Tag resolution ─────────────────────────────────────────

    /// Resolve a tag's raw bytes. For Inline, returns the bytes directly.
    /// For Lazy, performs a pread on first access and caches the result
    /// (including errors) permanently via OnceLock.
    pub fn resolve_tag(&self, ifd_id: IfdId, tag: u16) -> Result<&[u8], TiffParseError> {
        let ifd = self.ifd_by_id(ifd_id)?;
        let entry = ifd.tags.get(&tag).ok_or(TiffParseError::TagNotFound {
            ifd_offset: ifd.offset,
            tag,
        })?;

        match &entry.value {
            TagValue::Inline(v) => Ok(v.as_bytes()),
            TagValue::Lazy {
                offset,
                byte_len,
                resolved,
            } => {
                let tag_offset = *offset;
                let tag_byte_len = *byte_len;
                let result = resolved.get_or_init(|| self.pread(tag_offset, tag_byte_len));
                result.as_ref().map(|v| v.as_slice()).map_err(|e| e.clone())
            }
        }
    }

    /// Alias for resolve_tag — returns raw bytes without type interpretation.
    pub fn get_bytes(&self, ifd_id: IfdId, tag: u16) -> Result<&[u8], TiffParseError> {
        self.resolve_tag(ifd_id, tag)
    }

    // ── Typed scalar accessors ─────────────────────────────────

    /// Read a single u32 value. Accepts BYTE, SHORT, LONG types.
    pub fn get_u32(&self, ifd_id: IfdId, tag: u16) -> Result<u32, TiffParseError> {
        let ifd = self.ifd_by_id(ifd_id)?;
        let entry = ifd.tags.get(&tag).ok_or(TiffParseError::TagNotFound {
            ifd_offset: ifd.offset,
            tag,
        })?;
        if entry.count != 1 {
            return Err(TiffParseError::InvalidTag {
                ifd_offset: ifd.offset,
                tag,
                message: format!("expected count=1, got {}", entry.count),
            });
        }
        let bytes = self.resolve_tag(ifd_id, tag)?;
        match entry.tiff_type {
            TiffType::Byte => Ok(bytes[0] as u32),
            TiffType::Short => {
                let val = match self.endian {
                    Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
                    Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
                };
                Ok(val as u32)
            }
            TiffType::Long => {
                let val = match self.endian {
                    Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                    Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                };
                Ok(val)
            }
            _ => Err(TiffParseError::InvalidTag {
                ifd_offset: ifd.offset,
                tag,
                message: format!("get_u32 expects BYTE/SHORT/LONG, got {:?}", entry.tiff_type),
            }),
        }
    }

    /// Read a single u64 value. Accepts BYTE, SHORT, LONG, LONG8 types.
    pub fn get_u64(&self, ifd_id: IfdId, tag: u16) -> Result<u64, TiffParseError> {
        let ifd = self.ifd_by_id(ifd_id)?;
        let entry = ifd.tags.get(&tag).ok_or(TiffParseError::TagNotFound {
            ifd_offset: ifd.offset,
            tag,
        })?;
        if entry.count != 1 {
            return Err(TiffParseError::InvalidTag {
                ifd_offset: ifd.offset,
                tag,
                message: format!("expected count=1, got {}", entry.count),
            });
        }
        let bytes = self.resolve_tag(ifd_id, tag)?;
        match entry.tiff_type {
            TiffType::Byte => Ok(bytes[0] as u64),
            TiffType::Short => {
                let val = match self.endian {
                    Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
                    Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
                };
                Ok(val as u64)
            }
            TiffType::Long => {
                let val = match self.endian {
                    Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                    Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                };
                let val = val as u64;
                if self.ndpi && is_ndpi_data_offset_tag(tag) {
                    Ok(self.fix_ndpi_data_offset(ifd.offset, val))
                } else {
                    Ok(val)
                }
            }
            TiffType::Long8 => {
                let val = match self.endian {
                    Endian::Little => u64::from_le_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7],
                    ]),
                    Endian::Big => u64::from_be_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7],
                    ]),
                };
                Ok(val)
            }
            _ => Err(TiffParseError::InvalidTag {
                ifd_offset: ifd.offset,
                tag,
                message: format!(
                    "get_u64 expects BYTE/SHORT/LONG/LONG8, got {:?}",
                    entry.tiff_type
                ),
            }),
        }
    }

    /// Read a single f64 value. Accepts FLOAT, DOUBLE, RATIONAL, SRATIONAL types.
    pub fn get_f64(&self, ifd_id: IfdId, tag: u16) -> Result<f64, TiffParseError> {
        let ifd = self.ifd_by_id(ifd_id)?;
        let entry = ifd.tags.get(&tag).ok_or(TiffParseError::TagNotFound {
            ifd_offset: ifd.offset,
            tag,
        })?;
        if entry.count != 1 {
            return Err(TiffParseError::InvalidTag {
                ifd_offset: ifd.offset,
                tag,
                message: format!("expected count=1, got {}", entry.count),
            });
        }
        let bytes = self.resolve_tag(ifd_id, tag)?;
        match entry.tiff_type {
            TiffType::Float => {
                let val = match self.endian {
                    Endian::Little => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                    Endian::Big => f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                };
                Ok(val as f64)
            }
            TiffType::Double => {
                let val = match self.endian {
                    Endian::Little => f64::from_le_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7],
                    ]),
                    Endian::Big => f64::from_be_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7],
                    ]),
                };
                Ok(val)
            }
            TiffType::Rational => {
                let (num, den) = self.decode_rational(bytes)?;
                if den == 0 {
                    Ok(0.0)
                } else {
                    Ok(num as f64 / den as f64)
                }
            }
            TiffType::SRational => {
                let (num, den) = self.decode_srational(bytes)?;
                if den == 0 {
                    Ok(0.0)
                } else {
                    Ok(num as f64 / den as f64)
                }
            }
            _ => Err(TiffParseError::InvalidTag {
                ifd_offset: ifd.offset,
                tag,
                message: format!(
                    "get_f64 expects FLOAT/DOUBLE/RATIONAL/SRATIONAL, got {:?}",
                    entry.tiff_type
                ),
            }),
        }
    }

    /// Read an ASCII string value.
    pub fn get_string(&self, ifd_id: IfdId, tag: u16) -> Result<&str, TiffParseError> {
        let ifd = self.ifd_by_id(ifd_id)?;
        let entry = ifd.tags.get(&tag).ok_or(TiffParseError::TagNotFound {
            ifd_offset: ifd.offset,
            tag,
        })?;
        if entry.tiff_type != TiffType::Ascii {
            return Err(TiffParseError::InvalidTag {
                ifd_offset: ifd.offset,
                tag,
                message: format!("get_string expects ASCII, got {:?}", entry.tiff_type),
            });
        }
        let bytes = self.resolve_tag(ifd_id, tag)?;
        // Strip null terminator(s)
        let trimmed = match bytes.iter().position(|&b| b == 0) {
            Some(pos) => &bytes[..pos],
            None => bytes,
        };
        std::str::from_utf8(trimmed).map_err(|e| TiffParseError::InvalidTag {
            ifd_offset: ifd.offset,
            tag,
            message: format!("invalid UTF-8 in ASCII tag: {}", e),
        })
    }

    // ── Internal decode helpers ────────────────────────────────

    fn decode_rational(&self, bytes: &[u8]) -> Result<(u32, u32), TiffParseError> {
        if bytes.len() < 8 {
            return Err(TiffParseError::Structure(
                "RATIONAL requires 8 bytes".into(),
            ));
        }
        let num = match self.endian {
            Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        };
        let den = match self.endian {
            Endian::Little => u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            Endian::Big => u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        };
        Ok((num, den))
    }

    fn decode_srational(&self, bytes: &[u8]) -> Result<(i32, i32), TiffParseError> {
        if bytes.len() < 8 {
            return Err(TiffParseError::Structure(
                "SRATIONAL requires 8 bytes".into(),
            ));
        }
        let num = match self.endian {
            Endian::Little => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Endian::Big => i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        };
        let den = match self.endian {
            Endian::Little => i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            Endian::Big => i32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        };
        Ok((num, den))
    }

    // ── Typed array accessors (cached decode) ──────────────────

    /// Read a u64 array. Accepts SHORT, LONG, LONG8 types. Cached via OnceLock.
    pub fn get_u64_array(&self, ifd_id: IfdId, tag: u16) -> Result<&[u64], TiffParseError> {
        let ifd = self.ifd_by_id(ifd_id)?;
        let entry = ifd.tags.get(&tag).ok_or(TiffParseError::TagNotFound {
            ifd_offset: ifd.offset,
            tag,
        })?;

        if let Some(cached) = entry.decoded_u64.get() {
            return Ok(cached.as_slice());
        }

        let bytes = self.resolve_tag(ifd_id, tag)?;
        let count = entry.count as usize;

        let decoded: Vec<u64> = match entry.tiff_type {
            TiffType::Short => bytes
                .chunks_exact(2)
                .take(count)
                .map(|c| match self.endian {
                    Endian::Little => u16::from_le_bytes([c[0], c[1]]) as u64,
                    Endian::Big => u16::from_be_bytes([c[0], c[1]]) as u64,
                })
                .collect(),
            TiffType::Long => bytes
                .chunks_exact(4)
                .take(count)
                .map(|c| match self.endian {
                    Endian::Little => u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as u64,
                    Endian::Big => u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as u64,
                })
                .map(|value| {
                    if self.ndpi && is_ndpi_data_offset_tag(tag) {
                        self.fix_ndpi_data_offset(ifd.offset, value)
                    } else {
                        value
                    }
                })
                .collect(),
            TiffType::Long8 => bytes
                .chunks_exact(8)
                .take(count)
                .map(|c| match self.endian {
                    Endian::Little => {
                        u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])
                    }
                    Endian::Big => {
                        u64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])
                    }
                })
                .collect(),
            _ => {
                return Err(TiffParseError::InvalidTag {
                    ifd_offset: ifd.offset,
                    tag,
                    message: format!(
                        "get_u64_array expects SHORT/LONG/LONG8, got {:?}",
                        entry.tiff_type
                    ),
                });
            }
        };

        Ok(entry.decoded_u64.get_or_init(|| decoded).as_slice())
    }

    // ── Metadata accessors ─────────────────────────────────────

    pub fn path(&self) -> &Path {
        self.path.as_ref()
    }

    pub fn endian(&self) -> Endian {
        self.endian
    }

    pub fn is_bigtiff(&self) -> bool {
        self.bigtiff
    }

    pub fn is_ndpi(&self) -> bool {
        self.ndpi
    }

    pub fn ifd_count(&self) -> usize {
        self.ifds.len()
    }

    pub fn top_ifds(&self) -> &[IfdId] {
        &self.top_ifds
    }

    pub fn ifd_by_id(&self, id: IfdId) -> Result<&Ifd, TiffParseError> {
        self.ifds.get(&id).ok_or(TiffParseError::IfdNotFound(id))
    }

    fn fix_ndpi_data_offset(&self, ifd_offset: u64, raw_offset: u64) -> u64 {
        let fixed = fix_offset_ndpi(ifd_offset, raw_offset);
        for candidate in [
            raw_offset,
            raw_offset.saturating_sub(1),
            fixed,
            fixed.saturating_sub(1),
        ] {
            if candidate < self.file_len && self.looks_like_jpeg_soi(candidate) {
                return candidate;
            }
        }
        fixed
    }

    fn looks_like_jpeg_soi(&self, offset: u64) -> bool {
        self.pread(offset, 2)
            .is_ok_and(|bytes| bytes.as_slice() == [0xFF, 0xD8])
    }
}
