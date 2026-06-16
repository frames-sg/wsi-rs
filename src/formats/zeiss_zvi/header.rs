use super::compound::read_stream_prefix;
use super::model::{ZviCompression, ZviImageHeader};
use super::*;

const ZVI_HEADER_PROBE_BYTES: usize = 4096;

pub(super) fn read_zvi_header(
    compound: &mut CompoundFile<File>,
    stream_path: &str,
) -> Result<ZviImageHeader, WsiError> {
    let data = read_stream_prefix(compound, stream_path, ZVI_HEADER_PROBE_BYTES)?;
    parse_zvi_header(&data)
}

fn parse_zvi_header(data: &[u8]) -> Result<ZviImageHeader, WsiError> {
    let mut reader = ByteReader::new(data);
    for _ in 0..11 {
        let _ = reader.read_variant()?;
    }
    reader.skip(2)?;
    let coord_len = reader.read_i32()?.saturating_sub(20);
    reader.skip(8)?;
    let z = checked_axis(reader.read_i32()?)?;
    let c = checked_axis(reader.read_i32()?)?;
    let t = checked_axis(reader.read_i32()?)?;
    reader.skip(4)?;
    let tile_index = reader.read_i32()?;
    if coord_len < 8 {
        return Err(WsiError::DisplayConversion(
            "ZVI coordinate block is too short".into(),
        ));
    }
    reader.skip((coord_len - 8) as usize)?;
    for _ in 0..5 {
        let _ = reader.read_variant()?;
    }
    reader.skip(4)?;
    let width = checked_dimension(reader.read_i32()?)?;
    let height = checked_dimension(reader.read_i32()?)?;
    reader.skip(4)?;
    let bytes_per_sample = checked_dimension(reader.read_i32()?)?;
    reader.skip(4)?;
    let valid = reader.read_i32()?;
    let payload_offset = reader.position() as u64;
    let check = reader.read_bytes(4)?;
    let compression = if matches!(valid, 0 | 1) {
        if check == b"WZL\0" || &check[..3] == b"WZL" {
            ZviCompression::Zlib
        } else {
            ZviCompression::Jpeg
        }
    } else {
        ZviCompression::Raw
    };
    let payload_offset = if compression == ZviCompression::Zlib {
        payload_offset + 8
    } else {
        payload_offset
    };

    Ok(ZviImageHeader {
        width,
        height,
        bytes_per_sample,
        payload_offset,
        compression,
        z,
        c,
        t,
        tile_index,
    })
}

pub(super) struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(super) fn position(&self) -> usize {
        self.pos
    }

    pub(super) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub(super) fn skip(&mut self, count: usize) -> Result<(), WsiError> {
        self.require(count)?;
        self.pos += count;
        Ok(())
    }

    pub(super) fn read_bytes(&mut self, count: usize) -> Result<&'a [u8], WsiError> {
        self.require(count)?;
        let start = self.pos;
        self.pos += count;
        Ok(&self.data[start..self.pos])
    }

    fn read_u16(&mut self) -> Result<u16, WsiError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_i16(&mut self) -> Result<i16, WsiError> {
        let bytes = self.read_bytes(2)?;
        Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub(super) fn read_i32(&mut self) -> Result<i32, WsiError> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u32(&mut self) -> Result<u32, WsiError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_i64(&mut self) -> Result<i64, WsiError> {
        let bytes = self.read_bytes(8)?;
        Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_f32(&mut self) -> Result<f32, WsiError> {
        let bytes = self.read_bytes(4)?;
        Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_f64(&mut self) -> Result<f64, WsiError> {
        let bytes = self.read_bytes(8)?;
        Ok(f64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub(super) fn read_variant(&mut self) -> Result<String, WsiError> {
        let value_type = self.read_u16()?;
        match value_type {
            0 | 1 => Ok(String::new()),
            2 => self.read_i16().map(|value| value.to_string()),
            3 | 22 => self.read_i32().map(|value| value.to_string()),
            19 | 23 => self.read_u32().map(|value| value.to_string()),
            4 => self.read_f32().map(|value| value.to_string()),
            5 | 7 => self.read_f64().map(|value| value.to_string()),
            8 | 69 => {
                let byte_len = self.read_u32()? as usize;
                let raw = self.read_bytes(byte_len)?;
                Ok(decode_utf16le_lossy(raw))
            }
            9 | 13 => {
                self.skip(16)?;
                Ok(String::new())
            }
            11 => self.read_i16().map(|value| (value != 0).to_string()),
            20 => self.read_i64().map(|value| value.to_string()),
            21 => {
                let raw = self.read_bytes(8)?;
                Ok(u64::from_be_bytes([
                    raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                ])
                .to_string())
            }
            63 | 65 => {
                let byte_len = self.read_u32()? as usize;
                self.skip(byte_len)?;
                Ok(String::new())
            }
            66 => {
                let byte_len = self.read_u16()? as usize;
                let raw = self.read_bytes(byte_len)?;
                Ok(String::from_utf8_lossy(raw).into_owned())
            }
            _ => self.read_unknown_variant_lossy(),
        }
    }

    fn read_unknown_variant_lossy(&mut self) -> Result<String, WsiError> {
        let start = self.pos.saturating_sub(2);
        let mut scan = self.pos;
        while scan + 2 <= self.data.len() {
            if u16::from_le_bytes([self.data[scan], self.data[scan + 1]]) == 3 {
                break;
            }
            scan += 2;
        }
        self.pos = scan;
        Ok(decode_utf16le_lossy(&self.data[start..scan]))
    }

    fn require(&self, count: usize) -> Result<(), WsiError> {
        if self.remaining() < count {
            return Err(WsiError::DisplayConversion(
                "unexpected end of ZVI metadata stream".into(),
            ));
        }
        Ok(())
    }
}

fn checked_axis(value: i32) -> Result<u32, WsiError> {
    u32::try_from(value)
        .map_err(|_| WsiError::DisplayConversion("negative ZVI axis coordinate".into()))
}

fn checked_dimension(value: i32) -> Result<u32, WsiError> {
    let value = u32::try_from(value)
        .map_err(|_| WsiError::DisplayConversion("negative ZVI dimension".into()))?;
    if value == 0 {
        return Err(WsiError::DisplayConversion("zero ZVI dimension".into()));
    }
    Ok(value)
}

pub(super) fn decode_utf16le_lossy(raw: &[u8]) -> String {
    let words = raw
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .take_while(|value| *value != 0)
        .collect::<Vec<_>>();
    String::from_utf16_lossy(&words)
}
