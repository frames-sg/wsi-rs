//! Absolute file ranges of compound-file streams.
//!
//! The `cfb` crate reads streams but does not say where their bytes are.
//! This module reads the compound file's header, DIFAT, FAT, MiniFAT and
//! directory with bounded, checked reads and maps a range of a stream to the
//! file ranges that hold it, following MS-CFB the way `cfb` reads it: regular
//! streams through the FAT chain of whole sectors, streams under 4096 bytes
//! through the MiniFAT chain of 64-byte sectors inside the root entry's mini
//! stream. It reads metadata only, never stream contents.

use std::collections::{HashMap, HashSet};
use std::fs::File;

use crate::core::positioned_file::read_exact_at;
use crate::core::types::ByteRange;

const CFB_MAGIC: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];
const HEADER_LEN: usize = 512;
const NUM_DIFAT_ENTRIES_IN_HEADER: usize = 109;
const MAX_REGULAR_SECTOR: u32 = 0xffff_fffa;
const END_OF_CHAIN: u32 = 0xffff_fffe;
const FREE_SECTOR: u32 = 0xffff_ffff;
const NO_STREAM: u32 = 0xffff_ffff;
const DIR_ENTRY_LEN: usize = 128;
const MINI_SECTOR_LEN: u64 = 64;
const MINI_STREAM_CUTOFF: u64 = 4096;
const OBJ_TYPE_STORAGE: u8 = 1;
const OBJ_TYPE_STREAM: u8 = 2;
const OBJ_TYPE_ROOT: u8 = 5;

/// A stream's directory record.
#[derive(Clone, Copy, Debug)]
struct StreamRecord {
    start_sector: u32,
    len: u64,
}

/// The compound file's allocation tables and stream directory.
pub(super) struct CfbSectorMap<'a> {
    file: &'a File,
    file_len: u64,
    sector_len: u64,
    num_sectors: u32,
    /// Sector numbers of the FAT sectors, in FAT order.
    fat_sectors: Vec<u32>,
    /// Sector numbers of the MiniFAT sectors, in MiniFAT order.
    minifat_sectors: Vec<u32>,
    /// Sector numbers of the mini stream, in stream order.
    mini_stream_sectors: Vec<u32>,
    mini_stream_len: u64,
    /// The directory tree: each reachable entry by its parent entry and
    /// name, `None` when a storage holds two entries of that name.
    children: HashMap<(u32, String), Option<u32>>,
    /// Object type and stream record of each directory entry.
    nodes: Vec<(u8, StreamRecord)>,
    /// One cached table sector: which table and sector, and its entries.
    table_cache: Option<(u32, Vec<u32>)>,
}

impl<'a> CfbSectorMap<'a> {
    /// Reads the tables of the compound file open as `file`. The directory may
    /// list at most `max_directory_entries` entries, and its index may hold
    /// at most `max_index_bytes`.
    pub(super) fn read(
        file: &'a File,
        file_len: u64,
        max_directory_entries: usize,
        max_index_bytes: u64,
    ) -> Result<Self, String> {
        let mut header = [0u8; HEADER_LEN];
        read_at(file, file_len, 0, &mut header)?;
        if header[0..8] != CFB_MAGIC {
            return Err("compound file signature is missing".into());
        }
        let sector_shift = match (le_u16(&header, 0x1a), le_u16(&header, 0x1e)) {
            (3, 9) => 9,
            (4, 12) => 12,
            (version, shift) => {
                return Err(format!(
                    "compound file version {version} with sector shift {shift} is not supported"
                ))
            }
        };
        if le_u16(&header, 0x20) != 6 || u64::from(le_u32(&header, 0x38)) != MINI_STREAM_CUTOFF {
            return Err("compound file mini sector size or cutoff is not standard".into());
        }
        let sector_len = 1u64 << sector_shift;
        if file_len < sector_len {
            return Err("compound file is shorter than one sector".into());
        }
        let num_sectors = u32::try_from(file_len.div_ceil(sector_len) - 1)
            .ok()
            .filter(|count| *count <= MAX_REGULAR_SECTOR)
            .ok_or("compound file has too many sectors")?;
        let mut map = Self {
            file,
            file_len,
            sector_len,
            num_sectors,
            fat_sectors: Vec::new(),
            minifat_sectors: Vec::new(),
            mini_stream_sectors: Vec::new(),
            mini_stream_len: 0,
            children: HashMap::new(),
            nodes: Vec::new(),
            table_cache: None,
        };
        map.read_difat(&header)?;
        let directory = map.regular_chain(le_u32(&header, 0x30), None)?;
        let root = map.read_directory(&directory, max_directory_entries, max_index_bytes)?;
        map.minifat_sectors = map.regular_chain(le_u32(&header, 0x3c), None)?;
        map.mini_stream_len = root.len;
        let mini_sectors = root.len.div_ceil(sector_len);
        map.mini_stream_sectors = map.regular_chain(root.start_sector, Some(mini_sectors))?;
        Ok(map)
    }

    /// The merged file ranges holding bytes `offset..offset + len` of the
    /// stream at `path`, after checking that the directory gives the stream
    /// `expected_len` bytes.
    pub(super) fn stream_ranges(
        &mut self,
        path: &str,
        expected_len: u64,
        offset: u64,
        len: u64,
    ) -> Result<Vec<ByteRange>, String> {
        let record = self.stream_record(path)?;
        if record.len != expected_len {
            return Err(format!(
                "compound directory gives stream {path} {} bytes, but it reads as {expected_len}",
                record.len
            ));
        }
        let end = offset
            .checked_add(len)
            .filter(|end| *end <= record.len)
            .ok_or_else(|| format!("range {offset}+{len} is outside stream {path}"))?;
        let mini = record.len < MINI_STREAM_CUTOFF;
        let unit = if mini {
            MINI_SECTOR_LEN
        } else {
            self.sector_len
        };
        let mut ranges: Vec<ByteRange> = Vec::new();
        if len == 0 {
            return Ok(ranges);
        }
        let first = offset / unit;
        let last = (end - 1) / unit;
        let mut sector = record.start_sector;
        let mut seen = HashSet::new();
        for index in 0..=last {
            let limit = if mini {
                self.mini_stream_len / MINI_SECTOR_LEN
            } else {
                u64::from(self.num_sectors)
            };
            if u64::from(sector) >= limit || !seen.insert(sector) {
                return Err(format!(
                    "stream {path} chain has invalid or repeated sector {sector}"
                ));
            }
            if index >= first {
                let start = (index * unit).max(offset);
                let stop = ((index + 1) * unit).min(end);
                let absolute = if mini {
                    self.mini_offset(u64::from(sector) * MINI_SECTOR_LEN)?
                } else {
                    self.sector_offset(sector)
                } + (start - index * unit);
                push_merged(&mut ranges, ByteRange::new(absolute, stop - start));
            }
            if index < last {
                sector = if mini {
                    self.minifat_next(sector)?
                } else {
                    self.fat_next(sector)?
                };
            }
        }
        if ranges
            .iter()
            .any(|range| range.end().is_none_or(|end| end > self.file_len))
        {
            return Err(format!("stream {path} extends past the end of the file"));
        }
        Ok(ranges)
    }

    fn read_difat(&mut self, header: &[u8; HEADER_LEN]) -> Result<(), String> {
        let per_sector = self.entries_per_sector();
        let needed = u64::from(self.num_sectors).div_ceil(per_sector as u64) as usize;
        let mut entries = (0..NUM_DIFAT_ENTRIES_IN_HEADER)
            .map(|index| le_u32(header, 0x4c + index * 4))
            .collect::<Vec<_>>();
        let mut next = le_u32(header, 0x44);
        let mut seen = HashSet::new();
        while next != END_OF_CHAIN && next != FREE_SECTOR && entries.len() < needed {
            if next >= self.num_sectors || !seen.insert(next) {
                return Err(format!("DIFAT chain has invalid or repeated sector {next}"));
            }
            let sector = self.read_sector_entries(next)?;
            entries.extend_from_slice(&sector[..per_sector - 1]);
            next = sector[per_sector - 1];
        }
        entries.truncate(needed);
        self.fat_sectors = entries;
        Ok(())
    }

    /// Reads the directory and indexes each entry reachable from the root by
    /// its parent and name. Returns the root entry, whose chain is the mini
    /// stream. The index keeps each name once, so deep storage chains cost
    /// as much as wide ones; entries and names count against `max_bytes`.
    fn read_directory(
        &mut self,
        sectors: &[u32],
        max_entries: usize,
        max_bytes: u64,
    ) -> Result<StreamRecord, String> {
        let per_sector = self.sector_len as usize / DIR_ENTRY_LEN;
        if sectors.len().saturating_mul(per_sector) > max_entries {
            return Err(format!(
                "compound directory exceeds the {max_entries}-entry limit"
            ));
        }
        let mut retained = 0u64;
        let mut retain = |bytes: usize| {
            retained = retained.saturating_add(bytes as u64);
            if retained > max_bytes {
                return Err(format!(
                    "compound directory index exceeds the {max_bytes}-byte limit"
                ));
            }
            Ok(())
        };
        let mut entries = Vec::new();
        let mut sector_bytes = vec![0u8; self.sector_len as usize];
        for &sector in sectors {
            self.read_sector(sector, &mut sector_bytes)?;
            for raw in sector_bytes.as_chunks::<DIR_ENTRY_LEN>().0 {
                let entry = DirEntry::parse(raw, self.sector_len)?;
                retain(
                    std::mem::size_of::<DirEntry>()
                        + std::mem::size_of::<(u8, StreamRecord)>()
                        + entry.name.len(),
                )?;
                entries.push(entry);
            }
        }
        let root = entries
            .first()
            .filter(|root| root.obj_type == OBJ_TYPE_ROOT)
            .ok_or("compound directory has no root entry")?;
        let root_record = StreamRecord {
            start_sector: root.start_sector,
            len: root.len,
        };
        self.nodes = entries
            .iter()
            .map(|entry| {
                (
                    entry.obj_type,
                    StreamRecord {
                        start_sector: entry.start_sector,
                        len: entry.len,
                    },
                )
            })
            .collect();
        let index_entry = std::mem::size_of::<((u32, String), Option<u32>)>();
        // Each pending entry is an entry ID and the storage it belongs to.
        let mut pending = Vec::new();
        if root.child != NO_STREAM {
            pending.push((root.child, 0u32));
        }
        let mut visited = HashSet::from([0u32]);
        while let Some((id, parent)) = pending.pop() {
            let entry = entries
                .get_mut(id as usize)
                .filter(|_| visited.insert(id))
                .ok_or_else(|| format!("compound directory entry {id} is invalid or repeated"))?;
            for sibling in [entry.left, entry.right] {
                if sibling != NO_STREAM {
                    pending.push((sibling, parent));
                }
            }
            if entry.obj_type == OBJ_TYPE_STORAGE && entry.child != NO_STREAM {
                pending.push((entry.child, id));
            }
            if matches!(entry.obj_type, OBJ_TYPE_STORAGE | OBJ_TYPE_STREAM) {
                retain(index_entry)?;
                self.children
                    .entry((parent, std::mem::take(&mut entry.name)))
                    .and_modify(|slot| *slot = None)
                    .or_insert(Some(id));
            }
        }
        Ok(root_record)
    }

    /// The directory record of the stream at `path`, a `/`-joined path from
    /// the root such as `/Image/Item(0)/Contents`.
    fn stream_record(&self, path: &str) -> Result<StreamRecord, String> {
        let mut id = 0u32;
        let mut components = path.strip_prefix('/').unwrap_or(path).split('/').peekable();
        while let Some(name) = components.next() {
            let child = self
                .children
                .get(&(id, name.to_owned()))
                .ok_or_else(|| format!("compound directory has no stream {path}"))?
                .ok_or_else(|| {
                    format!("compound directory has two entries named {name} in {path}")
                })?;
            let expected = if components.peek().is_some() {
                OBJ_TYPE_STORAGE
            } else {
                OBJ_TYPE_STREAM
            };
            if self.nodes.get(child as usize).map(|(kind, _)| *kind) != Some(expected) {
                return Err(format!("compound directory has no stream {path}"));
            }
            id = child;
        }
        match self.nodes.get(id as usize) {
            Some(&(OBJ_TYPE_STREAM, record)) if id != 0 => Ok(record),
            _ => Err(format!("compound directory has no stream {path}")),
        }
    }

    /// The sectors of the regular chain from `start`: all of them, or the
    /// first `count`. A whole chain that starts at a free sector is empty.
    fn regular_chain(&mut self, start: u32, count: Option<u64>) -> Result<Vec<u32>, String> {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut sector = start;
        while count.is_none_or(|count| (chain.len() as u64) < count) {
            if count.is_none()
                && (sector == END_OF_CHAIN || (chain.is_empty() && sector == FREE_SECTOR))
            {
                break;
            }
            if sector >= self.num_sectors || !seen.insert(sector) {
                return Err(format!(
                    "allocation chain has invalid or repeated sector {sector}"
                ));
            }
            chain.push(sector);
            sector = self.fat_next(sector)?;
        }
        Ok(chain)
    }

    /// The FAT entry of `sector`.
    fn fat_next(&mut self, sector: u32) -> Result<u32, String> {
        let per_sector = self.entries_per_sector();
        let table_sector = *self
            .fat_sectors
            .get(sector as usize / per_sector)
            .ok_or_else(|| format!("sector {sector} is outside the FAT"))?;
        self.table_entry(table_sector, sector as usize % per_sector)
    }

    /// The MiniFAT entry of mini sector `sector`.
    fn minifat_next(&mut self, sector: u32) -> Result<u32, String> {
        let per_sector = self.entries_per_sector();
        let table_sector = *self
            .minifat_sectors
            .get(sector as usize / per_sector)
            .ok_or_else(|| format!("mini sector {sector} is outside the MiniFAT"))?;
        self.table_entry(table_sector, sector as usize % per_sector)
    }

    fn table_entry(&mut self, table_sector: u32, index: usize) -> Result<u32, String> {
        if self
            .table_cache
            .as_ref()
            .is_none_or(|(cached, _)| *cached != table_sector)
        {
            let entries = self.read_sector_entries(table_sector)?;
            self.table_cache = Some((table_sector, entries));
        }
        let (_, entries) = self.table_cache.as_ref().ok_or("table cache is empty")?;
        Ok(entries[index])
    }

    /// The file offset of byte `offset` of the mini stream.
    fn mini_offset(&self, offset: u64) -> Result<u64, String> {
        if offset >= self.mini_stream_len {
            return Err(format!("mini stream offset {offset} is past its end"));
        }
        let sector = self
            .mini_stream_sectors
            .get((offset / self.sector_len) as usize)
            .ok_or("mini stream chain is shorter than the mini stream")?;
        Ok(self.sector_offset(*sector) + offset % self.sector_len)
    }

    fn sector_offset(&self, sector: u32) -> u64 {
        (u64::from(sector) + 1) * self.sector_len
    }

    fn entries_per_sector(&self) -> usize {
        self.sector_len as usize / 4
    }

    fn read_sector_entries(&self, sector: u32) -> Result<Vec<u32>, String> {
        let mut bytes = vec![0u8; self.sector_len as usize];
        self.read_sector(sector, &mut bytes)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect())
    }

    fn read_sector(&self, sector: u32, bytes: &mut [u8]) -> Result<(), String> {
        if sector >= self.num_sectors {
            return Err(format!("table sector {sector} is outside the file"));
        }
        read_at(self.file, self.file_len, self.sector_offset(sector), bytes)
    }
}

/// The fields of a directory entry the stream map uses.
struct DirEntry {
    name: String,
    obj_type: u8,
    left: u32,
    right: u32,
    child: u32,
    start_sector: u32,
    len: u64,
}

impl DirEntry {
    fn parse(raw: &[u8], sector_len: u64) -> Result<Self, String> {
        let name_bytes = usize::from(le_u16(raw, 64));
        if name_bytes > 64 || name_bytes % 2 != 0 {
            return Err(format!(
                "compound directory name length {name_bytes} is invalid"
            ));
        }
        let name_chars = (name_bytes / 2).saturating_sub(1);
        let units = raw[..name_chars * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_le_bytes(*unit))
            .collect::<Vec<_>>();
        let name = String::from_utf16(&units)
            .map_err(|_| "compound directory name is not valid UTF-16".to_string())?;
        let raw_len = le_u64(raw, 120);
        // Version 3 files keep only the low 32 bits of a stream size.
        let len = if sector_len == 512 {
            raw_len & 0xffff_ffff
        } else {
            raw_len
        };
        Ok(Self {
            name,
            obj_type: raw[66],
            left: le_u32(raw, 68),
            right: le_u32(raw, 72),
            child: le_u32(raw, 76),
            start_sector: le_u32(raw, 116),
            len,
        })
    }
}

fn push_merged(ranges: &mut Vec<ByteRange>, range: ByteRange) {
    match ranges.last_mut() {
        Some(last) if last.end() == Some(range.offset) => last.length += range.length,
        _ => ranges.push(range),
    }
}

fn read_at(file: &File, file_len: u64, offset: u64, bytes: &mut [u8]) -> Result<(), String> {
    if offset
        .checked_add(bytes.len() as u64)
        .is_none_or(|end| end > file_len)
    {
        return Err(format!(
            "compound table read {offset}+{} is past the end of the file",
            bytes.len()
        ));
    }
    read_exact_at(file, bytes, offset).map_err(|error| error.to_string())
}

fn le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from(le_u32(bytes, offset)) | (u64::from(le_u32(bytes, offset + 4)) << 32)
}
