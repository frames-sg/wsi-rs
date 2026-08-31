use std::io::Write;

use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder};
use tempfile::NamedTempFile;

pub(crate) fn encode_test_jpeg(image: &image::RgbImage) -> Vec<u8> {
    let mut encoded = Vec::new();
    JpegEncoder::new(&mut encoded, 50)
        .encode(
            image.as_raw().as_slice(),
            image.width() as u16,
            image.height() as u16,
            JpegColorType::Rgb,
        )
        .unwrap();
    encoded
}

pub(crate) struct SyntheticTag {
    tag: u16,
    tiff_type: u16,
    count: u32,
    inline_value: [u8; 4],
    ool_data: Option<Vec<u8>>,
}

impl SyntheticTag {
    pub(crate) fn long(tag: u16, value: u32) -> Self {
        Self {
            tag,
            tiff_type: 4,
            count: 1,
            inline_value: value.to_le_bytes(),
            ool_data: None,
        }
    }

    pub(crate) fn long_array(tag: u16, values: &[u32]) -> Self {
        let data = values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        let mut inline_value = [0; 4];
        let ool_data = if data.len() <= inline_value.len() {
            inline_value[..data.len()].copy_from_slice(&data);
            None
        } else {
            Some(data)
        };
        Self {
            tag,
            tiff_type: 4,
            count: values.len() as u32,
            inline_value,
            ool_data,
        }
    }

    pub(crate) fn short(tag: u16, value: u16) -> Self {
        let mut inline_value = [0; 4];
        inline_value[..2].copy_from_slice(&value.to_le_bytes());
        Self {
            tag,
            tiff_type: 3,
            count: 1,
            inline_value,
            ool_data: None,
        }
    }

    pub(crate) fn ascii(tag: u16, value: &str) -> Self {
        let mut data = value.as_bytes().to_vec();
        data.push(0);
        Self {
            tag,
            tiff_type: 2,
            count: data.len() as u32,
            inline_value: [0; 4],
            ool_data: Some(data),
        }
    }

    pub(crate) fn bytes(tag: u16, data: Vec<u8>) -> Self {
        let count = data.len() as u32;
        let (inline_value, ool_data) = if data.len() <= 4 {
            let mut inline = [0; 4];
            inline[..data.len()].copy_from_slice(&data);
            (inline, None)
        } else {
            ([0; 4], Some(data))
        };
        Self {
            tag,
            tiff_type: 7,
            count,
            inline_value,
            ool_data,
        }
    }
}

pub(crate) fn build_tiff(ifds: &[Vec<SyntheticTag>]) -> NamedTempFile {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    let first_ifd_offset_pos = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes());

    let mut ool_offsets = Vec::new();
    for (ifd_idx, tags) in ifds.iter().enumerate() {
        for (tag_idx, tag) in tags.iter().enumerate() {
            if let Some(data) = &tag.ool_data {
                let offset = buf.len() as u32;
                buf.extend_from_slice(data);
                ool_offsets.push((ifd_idx, tag_idx, offset));
            }
        }
    }

    let mut ifd_offsets = Vec::new();
    let mut next_ifd_patch_positions = Vec::new();
    for (ifd_idx, tags) in ifds.iter().enumerate() {
        let ifd_offset = buf.len() as u32;
        ifd_offsets.push(ifd_offset);

        let mut sorted = tags.iter().enumerate().collect::<Vec<_>>();
        sorted.sort_by_key(|(_, tag)| tag.tag);

        buf.extend_from_slice(&(sorted.len() as u16).to_le_bytes());
        for (orig_idx, tag) in sorted {
            buf.extend_from_slice(&tag.tag.to_le_bytes());
            buf.extend_from_slice(&tag.tiff_type.to_le_bytes());
            buf.extend_from_slice(&tag.count.to_le_bytes());
            if tag.ool_data.is_some() {
                let offset = ool_offsets
                    .iter()
                    .find(|(ii, ti, _)| *ii == ifd_idx && *ti == orig_idx)
                    .map(|(_, _, offset)| *offset)
                    .unwrap();
                buf.extend_from_slice(&offset.to_le_bytes());
            } else {
                buf.extend_from_slice(&tag.inline_value);
            }
        }

        let next_pos = buf.len();
        buf.extend_from_slice(&0u32.to_le_bytes());
        next_ifd_patch_positions.push(next_pos);
    }

    if let Some(first_ifd_offset) = ifd_offsets.first() {
        buf[first_ifd_offset_pos..first_ifd_offset_pos + 4]
            .copy_from_slice(&first_ifd_offset.to_le_bytes());
    }
    for idx in 0..ifd_offsets.len().saturating_sub(1) {
        let pos = next_ifd_patch_positions[idx];
        buf[pos..pos + 4].copy_from_slice(&ifd_offsets[idx + 1].to_le_bytes());
    }

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}
