use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use byteorder::{BigEndian, LittleEndian, ReadBytesExt};
use tracing::debug;

use super::super::error::{IfdId, TiffParseError};
#[cfg(test)]
use super::model::TagValue;
use super::model::{tags, Endian, Ifd, TagEntry, TiffContainer, TiffType};
use super::ndpi_offsets::{fix_offset_ndpi, is_ndpi_extension, repair_ndpi_first_ifd_offset};

/// Maximum number of entries allowed in a single IFD. Prevents DoS from crafted
/// BigTIFF files with huge entry counts.
const MAX_IFD_ENTRIES: u64 = 100_000;
const MAX_TOTAL_IFD_ENTRIES: u64 = 2_000_000;

/// Maximum byte size for a single tag payload read. Prevents OOM from crafted
/// tags with enormous count × type_size products.
const MAX_TAG_PAYLOAD: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_TAG_PAYLOAD: u64 = 512 * 1024 * 1024;

// ── ParseReader (used only during open()) ──────────────────────────

/// Sequential reader used during TiffContainer::open().
/// Wraps a BufReader and provides endian-aware reading.
/// Dropped when open() returns — not stored on TiffContainer.
pub(super) struct ParseReader {
    reader: std::io::BufReader<std::fs::File>,
    endian: Endian,
    bigtiff: bool,
}

impl ParseReader {
    pub(super) fn new(file: std::fs::File, endian: Endian, bigtiff: bool) -> Self {
        ParseReader {
            reader: std::io::BufReader::new(file),
            endian,
            bigtiff,
        }
    }

    pub(super) fn read_u16(&mut self) -> Result<u16, TiffParseError> {
        let val = match self.endian {
            Endian::Little => self.reader.read_u16::<LittleEndian>()?,
            Endian::Big => self.reader.read_u16::<BigEndian>()?,
        };
        Ok(val)
    }

    pub(super) fn read_u32(&mut self) -> Result<u32, TiffParseError> {
        let val = match self.endian {
            Endian::Little => self.reader.read_u32::<LittleEndian>()?,
            Endian::Big => self.reader.read_u32::<BigEndian>()?,
        };
        Ok(val)
    }

    pub(super) fn read_u64(&mut self) -> Result<u64, TiffParseError> {
        let val = match self.endian {
            Endian::Little => self.reader.read_u64::<LittleEndian>()?,
            Endian::Big => self.reader.read_u64::<BigEndian>()?,
        };
        Ok(val)
    }

    pub(super) fn read_bytes(&mut self, len: usize) -> Result<Vec<u8>, TiffParseError> {
        use std::io::Read;
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf)?;
        Ok(buf)
    }

    pub(super) fn seek(&mut self, offset: u64) -> Result<(), TiffParseError> {
        use std::io::{Seek, SeekFrom};
        self.reader.seek(SeekFrom::Start(offset))?;
        Ok(())
    }
}

impl TiffContainer {
    /// Open and parse a TIFF or BigTIFF file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TiffParseError> {
        let started = Instant::now();
        let path = path.as_ref();
        let file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        let file_len = metadata.len();

        // We need at least 8 bytes for a classic TIFF header
        if file_len < 8 {
            return Err(TiffParseError::Structure(format!(
                "file too small for TIFF header: {} bytes",
                file_len
            )));
        }

        // Parse header with a sequential reader
        let mut reader = ParseReader::new(file, Endian::Little, false); // endian TBD

        // Read byte order mark
        let bom = reader.read_bytes(2)?;
        let endian = match (bom[0], bom[1]) {
            (b'I', b'I') => Endian::Little,
            (b'M', b'M') => Endian::Big,
            _ => {
                return Err(TiffParseError::Structure(format!(
                    "invalid TIFF byte order marker: [{:#04x}, {:#04x}]",
                    bom[0], bom[1]
                )));
            }
        };
        reader.endian = endian;

        // Read magic number
        let magic = reader.read_u16()?;
        let bigtiff = match magic {
            42 => false,
            43 => true,
            _ => {
                return Err(TiffParseError::Structure(format!(
                    "invalid TIFF magic number: {} (expected 42 or 43)",
                    magic
                )));
            }
        };
        reader.bigtiff = bigtiff;

        // BigTIFF: validate offset size and reserved field
        if bigtiff {
            let offset_size = reader.read_u16()?;
            if offset_size != 8 {
                return Err(TiffParseError::Structure(format!(
                    "invalid BigTIFF offset size: {} (expected 8)",
                    offset_size
                )));
            }
            let reserved = reader.read_u16()?;
            if reserved != 0 {
                return Err(TiffParseError::Structure(format!(
                    "invalid BigTIFF reserved field: {} (expected 0)",
                    reserved
                )));
            }
        }

        let initial_ndpi = is_ndpi_extension(path);

        // Read first IFD offset
        let raw_first_ifd_offset = if bigtiff {
            reader.read_u64()?
        } else {
            reader.read_u32()? as u64
        };
        let first_ifd_offset = if initial_ndpi && !bigtiff {
            repair_ndpi_first_ifd_offset(&mut reader, raw_first_ifd_offset, file_len)?
        } else {
            raw_first_ifd_offset
        };

        let path_arc = Arc::new(path.to_path_buf());

        // Re-open a dedicated handle for pread (the parse reader consumes
        // its File and we don't want to share seek state).
        let pread_file = std::fs::File::open(path)?;

        let mut container = TiffContainer {
            path: path_arc,
            file_len,
            file: pread_file,
            #[cfg(windows)]
            pread_lock: std::sync::Mutex::new(()),
            endian,
            bigtiff,
            ndpi: initial_ndpi,
            top_ifds: Vec::new(),
            ifds: HashMap::new(),
            parsed_ifd_entries: 0,
            declared_tag_payload_bytes: 0,
        };

        // Walk the IFD chain
        container.walk_ifd_chain(&mut reader, first_ifd_offset)?;

        debug!(
            path = %path.display(),
            file_len = container.file_len,
            top_ifd_count = container.top_ifds.len(),
            ifd_count = container.ifds.len(),
            bigtiff = container.bigtiff,
            ndpi = container.ndpi,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "parsed TIFF container"
        );

        Ok(container)
    }

    #[cfg(test)]
    fn open_parse_reader(&self) -> Result<ParseReader, TiffParseError> {
        let file = std::fs::File::open(self.path.as_ref())?;
        Ok(ParseReader::new(file, self.endian, self.bigtiff))
    }

    // ── IFD chain walking ──────────────────────────────────────

    fn walk_ifd_chain(
        &mut self,
        reader: &mut ParseReader,
        first_ifd_offset: u64,
    ) -> Result<(), TiffParseError> {
        let mut ifd_offset = first_ifd_offset;
        let mut is_first = true;

        while ifd_offset != 0 {
            // Global safety limit
            if self.ifds.len() >= 10_000 {
                return Err(TiffParseError::Structure(
                    "too many IFDs (>10000), possible corrupt file".into(),
                ));
            }

            // Main-chain loop detection
            let ifd_id = IfdId(ifd_offset);
            if self.ifds.contains_key(&ifd_id) {
                return Err(TiffParseError::Structure(format!(
                    "IFD chain loop: offset {} already visited",
                    ifd_offset
                )));
            }

            let (ifd, next_offset) = self.parse_ifd(reader, ifd_offset)?;

            // NDPI detection: tag 65420 in first IFD
            if is_first && ifd.tags.contains_key(&tags::NDPI_MARKER) {
                self.ndpi = true;
            }
            is_first = false;

            self.top_ifds.push(ifd_id);
            self.ifds.insert(ifd_id, ifd);

            ifd_offset = next_offset;
        }

        Ok(())
    }

    /// Parse a single IFD at the given offset. Returns the Ifd and the next-IFD offset.
    fn parse_ifd(
        &mut self,
        reader: &mut ParseReader,
        offset: u64,
    ) -> Result<(Ifd, u64), TiffParseError> {
        reader.seek(offset)?;

        // Read entry count
        let entry_count: u64 = if self.bigtiff {
            reader.read_u64()?
        } else {
            reader.read_u16()? as u64
        };

        if entry_count > MAX_IFD_ENTRIES {
            return Err(TiffParseError::Structure(format!(
                "IFD entry count {} exceeds maximum {}",
                entry_count, MAX_IFD_ENTRIES
            )));
        }
        self.parsed_ifd_entries = self
            .parsed_ifd_entries
            .checked_add(entry_count)
            .ok_or_else(|| {
                TiffParseError::Structure("aggregate TIFF IFD entry count overflow".into())
            })?;
        if self.parsed_ifd_entries > MAX_TOTAL_IFD_ENTRIES {
            return Err(TiffParseError::Structure(format!(
                "aggregate TIFF IFD entry count exceeds {MAX_TOTAL_IFD_ENTRIES}"
            )));
        }

        let slot_size: u64 = if self.bigtiff { 8 } else { 4 };
        let mut tags_map = HashMap::new();

        for _ in 0..entry_count {
            let tag_id = reader.read_u16()?;
            let type_id = reader.read_u16()?;

            let count: u64 = if self.bigtiff {
                reader.read_u64()?
            } else {
                reader.read_u32()? as u64
            };

            // Read the value/offset slot as raw bytes
            let slot_bytes = reader.read_bytes(slot_size as usize)?;

            // Try to interpret the type
            let tiff_type = match TiffType::from_u16(type_id) {
                Some(t) => t,
                None => {
                    // Unknown type — skip this entry
                    continue;
                }
            };

            let total_bytes = count.checked_mul(tiff_type.byte_size()).ok_or_else(|| {
                TiffParseError::InvalidTag {
                    ifd_offset: offset,
                    tag: tag_id,
                    message: format!(
                        "payload too large: count={}, type_size={}",
                        count,
                        tiff_type.byte_size()
                    ),
                }
            })?;

            if total_bytes > MAX_TAG_PAYLOAD {
                return Err(TiffParseError::InvalidTag {
                    ifd_offset: offset,
                    tag: tag_id,
                    message: format!(
                        "tag payload {} bytes exceeds {} byte limit",
                        total_bytes, MAX_TAG_PAYLOAD
                    ),
                });
            }
            self.declared_tag_payload_bytes = self
                .declared_tag_payload_bytes
                .checked_add(total_bytes)
                .ok_or_else(|| TiffParseError::InvalidTag {
                    ifd_offset: offset,
                    tag: tag_id,
                    message: "aggregate TIFF tag payload length overflow".into(),
                })?;
            if self.declared_tag_payload_bytes > MAX_TOTAL_TAG_PAYLOAD {
                return Err(TiffParseError::InvalidTag {
                    ifd_offset: offset,
                    tag: tag_id,
                    message: format!(
                        "aggregate TIFF tag payload exceeds {MAX_TOTAL_TAG_PAYLOAD} byte limit"
                    ),
                });
            }

            let entry = if total_bytes <= slot_size {
                // Inline
                TagEntry::new_inline(tiff_type, count, &slot_bytes[..total_bytes as usize])
            } else {
                // Out-of-line: interpret slot as offset
                let data_offset = if self.bigtiff {
                    let mut c = std::io::Cursor::new(&slot_bytes);
                    match self.endian {
                        Endian::Little => c.read_u64::<LittleEndian>()?,
                        Endian::Big => c.read_u64::<BigEndian>()?,
                    }
                } else {
                    let mut c = std::io::Cursor::new(&slot_bytes);
                    let raw = match self.endian {
                        Endian::Little => c.read_u32::<LittleEndian>()?,
                        Endian::Big => c.read_u32::<BigEndian>()?,
                    };
                    let off = raw as u64;
                    // Apply NDPI fixup for out-of-line classic TIFF offsets
                    if self.ndpi {
                        fix_offset_ndpi(offset, off)
                    } else {
                        off
                    }
                };

                TagEntry::new_lazy(tiff_type, count, data_offset, total_bytes)
            };

            tags_map.insert(tag_id, entry);
        }

        // Read next IFD offset
        let next_offset = if self.ndpi && !self.bigtiff {
            // NDPI: read 8-byte next-IFD pointers even in classic TIFF
            reader.read_u64()?
        } else if self.bigtiff {
            reader.read_u64()?
        } else {
            reader.read_u32()? as u64
        };

        let ifd = Ifd {
            id: IfdId(offset),
            offset,
            tags: tags_map,
            sub_ifds: Vec::new(),
        };

        Ok((ifd, next_offset))
    }

    // ── SubIFD parsing ────────────────────────────────────────

    /// Parse SubIFDs referenced by tag 330 in an IFD.
    /// Adds newly discovered IFDs to the global arena. Deduplicates by offset.
    #[cfg(test)]
    pub fn materialize_sub_ifds(
        &mut self,
        parent_ifd_id: IfdId,
        max_depth: u32,
    ) -> Result<(), TiffParseError> {
        let mut reader = self.open_parse_reader()?;
        let mut ancestry = vec![parent_ifd_id];
        self.parse_sub_ifds(&mut reader, parent_ifd_id, 0, max_depth, &mut ancestry)
    }

    #[cfg(test)]
    pub fn materialize_all_sub_ifds(&mut self, max_depth: u32) -> Result<(), TiffParseError> {
        let root_ids = self.top_ifds.clone();
        let mut reader = self.open_parse_reader()?;
        for root_id in root_ids {
            let mut ancestry = vec![root_id];
            self.parse_sub_ifds(&mut reader, root_id, 0, max_depth, &mut ancestry)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn parse_sub_ifds(
        &mut self,
        reader: &mut ParseReader,
        parent_ifd_id: IfdId,
        depth: u32,
        max_depth: u32,
        ancestry: &mut Vec<IfdId>,
    ) -> Result<(), TiffParseError> {
        if depth > max_depth {
            return Err(TiffParseError::Structure(format!(
                "SubIFD depth limit exceeded (max {})",
                max_depth
            )));
        }

        // Check if parent has a SUB_IFDS tag
        let sub_ifd_offsets = {
            let parent = self
                .ifds
                .get(&parent_ifd_id)
                .ok_or(TiffParseError::IfdNotFound(parent_ifd_id))?;

            let entry = match parent.tags.get(&tags::SUB_IFDS) {
                Some(e) => e,
                None => return Ok(()), // No SubIFDs
            };

            // Extract offsets from inline or lazy data
            let bytes = match &entry.value {
                TagValue::Inline(v) => v.as_bytes().to_vec(),
                TagValue::Lazy {
                    offset, byte_len, ..
                } => self.pread(*offset, *byte_len)?,
            };

            // Decode as array of IFD offsets
            let elem_size = entry.tiff_type.byte_size() as usize;
            if elem_size == 0 {
                return Ok(());
            }
            let mut offsets = Vec::new();
            for chunk in bytes.chunks_exact(elem_size) {
                let off = match (entry.tiff_type, self.endian) {
                    (TiffType::Long | TiffType::Ifd, Endian::Little) => {
                        u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as u64
                    }
                    (TiffType::Long | TiffType::Ifd, Endian::Big) => {
                        u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as u64
                    }
                    (TiffType::Long8 | TiffType::Ifd8, Endian::Little) => u64::from_le_bytes([
                        chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6],
                        chunk[7],
                    ]),
                    (TiffType::Long8 | TiffType::Ifd8, Endian::Big) => u64::from_be_bytes([
                        chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6],
                        chunk[7],
                    ]),
                    _ => continue,
                };
                if off != 0 {
                    offsets.push(off);
                }
            }
            offsets
        };

        // Parse each SubIFD
        let mut child_ids = Vec::new();
        for sub_offset in &sub_ifd_offsets {
            let sub_id = IfdId(*sub_offset);

            if ancestry.contains(&sub_id) {
                return Err(TiffParseError::Structure(format!(
                    "SubIFD loop detected: offset {} already in ancestry",
                    sub_offset
                )));
            }

            if !self.ifds.contains_key(&sub_id) {
                // Safety limit
                if self.ifds.len() >= 10_000 {
                    return Err(TiffParseError::Structure(
                        "too many IFDs (>10000), possible corrupt file".into(),
                    ));
                }

                let (ifd, _next_offset) = self.parse_ifd(reader, *sub_offset)?;
                self.ifds.insert(sub_id, ifd);
            }
            child_ids.push(sub_id);

            ancestry.push(sub_id);
            self.parse_sub_ifds(reader, sub_id, depth + 1, max_depth, ancestry)?;
            ancestry.pop();
        }

        // Store child IDs on the parent
        if let Some(parent) = self.ifds.get_mut(&parent_ifd_id) {
            parent.sub_ifds = child_ids;
        }

        Ok(())
    }
}
