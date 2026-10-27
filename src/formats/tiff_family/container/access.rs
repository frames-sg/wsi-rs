use std::path::Path;
#[cfg(windows)]
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

use super::super::error::{IfdId, TiffParseError};
use super::model::{Endian, Ifd, TagEntry, TagValue, TiffContainer, TiffType};
use super::ndpi_offsets::{fix_offset_ndpi, is_ndpi_data_offset_tag};

impl TiffContainer {
    pub(crate) fn limits(&self) -> crate::SlideLimits {
        self.open_budget.limits()
    }

    // ── pread (stateless positional read) ──────────────────────

    /// Perform a positional read using the persistent file handle.
    /// Uses pread(2) on Unix for lock-free concurrent reads (no seek state).
    pub(crate) fn pread(&self, offset: u64, len: u64) -> Result<Vec<u8>, TiffParseError> {
        // Bounds check with checked arithmetic
        let end = offset
            .checked_add(len)
            .ok_or(TiffParseError::Bounds { offset, len })?;
        if end > self.file_len {
            return Err(TiffParseError::Bounds { offset, len });
        }

        let len_usize = usize::try_from(len).map_err(|_| TiffParseError::Bounds { offset, len })?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(len_usize)
            .map_err(|_| TiffParseError::ResourceLimit {
                resource: "TIFF read allocation",
                requested: len,
                limit: self.file_len,
            })?;
        buf.resize(len_usize, 0);

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
    pub(crate) fn resolve_tag(&self, ifd_id: IfdId, tag: u16) -> Result<&[u8], TiffParseError> {
        let (_, entry) = self.tag_entry(ifd_id, tag)?;
        self.entry_bytes(entry)
    }

    /// [`Self::resolve_tag`] for an entry that has already been looked up.
    fn entry_bytes<'a>(&'a self, entry: &'a TagEntry) -> Result<&'a [u8], TiffParseError> {
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

    /// Alias for resolve_tag. Returns raw bytes without type interpretation.
    pub(crate) fn get_bytes(&self, ifd_id: IfdId, tag: u16) -> Result<&[u8], TiffParseError> {
        self.resolve_tag(ifd_id, tag)
    }

    // ── Typed scalar accessors ─────────────────────────────────

    /// Read a single u32 value. Accepts BYTE, SHORT, LONG types.
    pub(crate) fn get_u32(&self, ifd_id: IfdId, tag: u16) -> Result<u32, TiffParseError> {
        let (ifd_offset, tiff_type, bytes) = self.single_value(ifd_id, tag)?;
        match tiff_type {
            TiffType::Byte => Ok(bytes[0] as u32),
            TiffType::Short => Ok(read_u16(self.endian, bytes) as u32),
            TiffType::Long => Ok(read_u32(self.endian, bytes)),
            _ => Err(invalid_tag(
                ifd_offset,
                tag,
                format!("get_u32 expects BYTE/SHORT/LONG, got {:?}", tiff_type),
            )),
        }
    }

    /// Read a single u64 value. Accepts BYTE, SHORT, LONG, LONG8 types.
    pub(crate) fn get_u64(&self, ifd_id: IfdId, tag: u16) -> Result<u64, TiffParseError> {
        let (ifd_offset, tiff_type, bytes) = self.single_value(ifd_id, tag)?;
        match tiff_type {
            TiffType::Byte => Ok(bytes[0] as u64),
            TiffType::Short => Ok(read_u16(self.endian, bytes) as u64),
            TiffType::Long => {
                let val = read_u32(self.endian, bytes) as u64;
                if self.ndpi && is_ndpi_data_offset_tag(tag) {
                    Ok(self.fix_ndpi_data_offset(ifd_offset, val))
                } else {
                    Ok(val)
                }
            }
            TiffType::Long8 => Ok(read_u64(self.endian, bytes)),
            _ => Err(invalid_tag(
                ifd_offset,
                tag,
                format!("get_u64 expects BYTE/SHORT/LONG/LONG8, got {:?}", tiff_type),
            )),
        }
    }

    /// Read a single f64 value. Accepts FLOAT, DOUBLE, RATIONAL, SRATIONAL types.
    pub(crate) fn get_f64(&self, ifd_id: IfdId, tag: u16) -> Result<f64, TiffParseError> {
        let (ifd_offset, tiff_type, bytes) = self.single_value(ifd_id, tag)?;
        let value = match tiff_type {
            TiffType::Float => f32::from_bits(read_u32(self.endian, bytes)) as f64,
            TiffType::Double => f64::from_bits(read_u64(self.endian, bytes)),
            TiffType::Rational | TiffType::SRational => {
                let signed = tiff_type == TiffType::SRational;
                let (num, den) = self.decode_rational(bytes, signed)?;
                if den == 0 {
                    return Err(invalid_tag(
                        ifd_offset,
                        tag,
                        "rational denominator must be nonzero".to_string(),
                    ));
                }
                if signed {
                    num as i32 as f64 / den as i32 as f64
                } else {
                    num as f64 / den as f64
                }
            }
            _ => {
                return Err(invalid_tag(
                    ifd_offset,
                    tag,
                    format!(
                        "get_f64 expects FLOAT/DOUBLE/RATIONAL/SRATIONAL, got {:?}",
                        tiff_type
                    ),
                ));
            }
        };
        if value.is_finite() {
            Ok(value)
        } else {
            Err(invalid_tag(
                ifd_offset,
                tag,
                "floating-point value must be finite".to_string(),
            ))
        }
    }

    /// Read an ASCII string value.
    pub(crate) fn get_string(&self, ifd_id: IfdId, tag: u16) -> Result<&str, TiffParseError> {
        let (ifd_offset, entry) = self.tag_entry(ifd_id, tag)?;
        if entry.tiff_type != TiffType::Ascii {
            return Err(invalid_tag(
                ifd_offset,
                tag,
                format!("get_string expects ASCII, got {:?}", entry.tiff_type),
            ));
        }
        let bytes = self.entry_bytes(entry)?;
        // Strip null terminator(s)
        let trimmed = match bytes.iter().position(|&b| b == 0) {
            Some(pos) => &bytes[..pos],
            None => bytes,
        };
        std::str::from_utf8(trimmed).map_err(|e| {
            invalid_tag(
                ifd_offset,
                tag,
                format!("invalid UTF-8 in ASCII tag: {}", e),
            )
        })
    }

    // ── Internal decode helpers ────────────────────────────────

    /// The IFD offset and entry for `tag`, or `TagNotFound`.
    fn tag_entry(&self, ifd_id: IfdId, tag: u16) -> Result<(u64, &TagEntry), TiffParseError> {
        let ifd = self.ifd_by_id(ifd_id)?;
        let entry = ifd.tags.get(&tag).ok_or(TiffParseError::TagNotFound {
            ifd_offset: ifd.offset,
            tag,
        })?;
        Ok((ifd.offset, entry))
    }

    /// The IFD offset, type and raw bytes of a tag that must hold exactly one value.
    fn single_value(
        &self,
        ifd_id: IfdId,
        tag: u16,
    ) -> Result<(u64, TiffType, &[u8]), TiffParseError> {
        let (ifd_offset, entry) = self.tag_entry(ifd_id, tag)?;
        if entry.count != 1 {
            return Err(invalid_tag(
                ifd_offset,
                tag,
                format!("expected count=1, got {}", entry.count),
            ));
        }
        let bytes = self.entry_bytes(entry)?;
        Ok((ifd_offset, entry.tiff_type, bytes))
    }

    /// The raw numerator and denominator words of a RATIONAL or SRATIONAL.
    fn decode_rational(&self, bytes: &[u8], signed: bool) -> Result<(u32, u32), TiffParseError> {
        if bytes.len() < 8 {
            let kind = if signed { "SRATIONAL" } else { "RATIONAL" };
            return Err(TiffParseError::Structure(format!(
                "{kind} requires 8 bytes"
            )));
        }
        Ok((
            read_u32(self.endian, &bytes[..4]),
            read_u32(self.endian, &bytes[4..8]),
        ))
    }

    // ── Typed array accessors (cached decode) ──────────────────

    /// Read a u64 array. Accepts SHORT, LONG, LONG8 types, plus IFD and IFD8
    /// (offset-typed LONG/LONG8, as written for SubIFDs). Cached via OnceLock.
    pub(crate) fn get_u64_array(&self, ifd_id: IfdId, tag: u16) -> Result<&[u64], TiffParseError> {
        let (ifd_offset, entry) = self.tag_entry(ifd_id, tag)?;

        if let Some(cached) = entry.decoded_u64.get() {
            return Ok(cached.as_slice());
        }

        let bytes = self.entry_bytes(entry)?;
        let count = entry.count as usize;

        let decoded: Vec<u64> = match entry.tiff_type {
            TiffType::Short => bytes
                .as_chunks::<2>()
                .0
                .iter()
                .take(count)
                .map(|c| read_u16(self.endian, c) as u64)
                .collect(),
            TiffType::Long | TiffType::Ifd => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .take(count)
                .map(|c| read_u32(self.endian, c) as u64)
                .map(|value| {
                    if self.ndpi && is_ndpi_data_offset_tag(tag) {
                        self.fix_ndpi_data_offset(ifd_offset, value)
                    } else {
                        value
                    }
                })
                .collect(),
            TiffType::Long8 | TiffType::Ifd8 => bytes
                .as_chunks::<8>()
                .0
                .iter()
                .take(count)
                .map(|c| read_u64(self.endian, c))
                .collect(),
            _ => {
                return Err(invalid_tag(
                    ifd_offset,
                    tag,
                    format!(
                        "get_u64_array expects SHORT/LONG/LONG8/IFD/IFD8, got {:?}",
                        entry.tiff_type
                    ),
                ));
            }
        };

        Ok(entry.decoded_u64.get_or_init(|| decoded).as_slice())
    }

    // ── Metadata accessors ─────────────────────────────────────

    pub(crate) fn path(&self) -> &Path {
        self.path.as_ref()
    }

    pub(crate) fn endian(&self) -> Endian {
        self.endian
    }

    pub(crate) fn is_bigtiff(&self) -> bool {
        self.bigtiff
    }

    pub(crate) fn is_ndpi(&self) -> bool {
        self.ndpi
    }

    #[cfg(test)]
    pub(crate) fn ifd_count(&self) -> usize {
        self.ifds.len()
    }

    pub(crate) fn top_ifds(&self) -> &[IfdId] {
        &self.top_ifds
    }

    pub(crate) fn ifd_by_id(&self, id: IfdId) -> Result<&Ifd, TiffParseError> {
        self.ifds.get(&id).ok_or(TiffParseError::IfdNotFound(id))
    }

    /// SubIFD children of a top-level IFD, in tag order; empty when it has none.
    pub(crate) fn sub_ifds(&self, parent: IfdId) -> &[IfdId] {
        self.sub_ifds.get(&parent).map_or(&[], Vec::as_slice)
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

fn invalid_tag(ifd_offset: u64, tag: u16, message: String) -> TiffParseError {
    TiffParseError::InvalidTag {
        ifd_offset,
        tag,
        message,
    }
}

fn read_u16(endian: Endian, bytes: &[u8]) -> u16 {
    let raw = [bytes[0], bytes[1]];
    match endian {
        Endian::Little => u16::from_le_bytes(raw),
        Endian::Big => u16::from_be_bytes(raw),
    }
}

fn read_u32(endian: Endian, bytes: &[u8]) -> u32 {
    let raw = [bytes[0], bytes[1], bytes[2], bytes[3]];
    match endian {
        Endian::Little => u32::from_le_bytes(raw),
        Endian::Big => u32::from_be_bytes(raw),
    }
}

fn read_u64(endian: Endian, bytes: &[u8]) -> u64 {
    let raw = [
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ];
    match endian {
        Endian::Little => u64::from_le_bytes(raw),
        Endian::Big => u64::from_be_bytes(raw),
    }
}
