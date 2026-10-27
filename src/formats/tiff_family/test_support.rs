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
    /// Payloads `build_tiff_tree` writes into the file; the tag's values are
    /// their offsets.
    blobs: Vec<Vec<u8>>,
}

impl SyntheticTag {
    pub(crate) fn long(tag: u16, value: u32) -> Self {
        Self {
            tag,
            tiff_type: 4,
            count: 1,
            inline_value: value.to_le_bytes(),
            ool_data: None,
            blobs: Vec::new(),
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
            blobs: Vec::new(),
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
            blobs: Vec::new(),
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
            blobs: Vec::new(),
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
            blobs: Vec::new(),
        }
    }

    /// BigTIFF LONG8 value. Write it with `build_bigtiff`, whose 8-byte value
    /// slot holds it inline.
    pub(crate) fn long8(tag: u16, value: u64) -> Self {
        Self {
            tag,
            tiff_type: 16,
            count: 1,
            inline_value: [0; 4],
            ool_data: Some(value.to_le_bytes().to_vec()),
            blobs: Vec::new(),
        }
    }

    /// LONG array holding the file offsets of `blobs`, which
    /// `build_tiff_tree` writes into the file. Use for tile or strip offsets.
    pub(crate) fn blob_offsets(tag: u16, blobs: Vec<Vec<u8>>) -> Self {
        Self {
            tag,
            tiff_type: 4,
            count: blobs.len() as u32,
            inline_value: [0; 4],
            ool_data: None,
            blobs,
        }
    }

    fn ifd_offsets(tag: u16, offsets: &[u32]) -> Self {
        Self {
            tiff_type: 13,
            ..Self::long_array(tag, offsets)
        }
    }
}

/// A top-level IFD for `build_tiff_tree` and the SubIFDs written beneath it.
pub(crate) struct SyntheticIfd {
    pub(crate) tags: Vec<SyntheticTag>,
    pub(crate) sub_ifds: Vec<Vec<SyntheticTag>>,
}

/// Build a little-endian classic TIFF whose top-level IFDs may carry SubIFDs
/// (tag 330, type IFD). All IFDs come first; out-of-line values and blobs follow.
pub(crate) fn build_tiff_tree(ifds: &[SyntheticIfd]) -> NamedTempFile {
    const HEADER_LEN: u32 = 8;
    let ifd_len = |tag_count: usize| 2 + 12 * tag_count as u32 + 4;

    let mut offset = HEADER_LEN;
    let mut top_offsets = Vec::new();
    let mut sub_offsets = Vec::new();
    for ifd in ifds {
        top_offsets.push(offset);
        offset += ifd_len(ifd.tags.len() + usize::from(!ifd.sub_ifds.is_empty()));
        let mut subs = Vec::new();
        for sub in &ifd.sub_ifds {
            subs.push(offset);
            offset += ifd_len(sub.len());
        }
        sub_offsets.push(subs);
    }
    let data_start = offset;

    let mut ifd_bytes = Vec::new();
    let mut data = Vec::new();
    for (index, ifd) in ifds.iter().enumerate() {
        let sub_tag = (!ifd.sub_ifds.is_empty()).then(|| {
            SyntheticTag::ifd_offsets(
                crate::formats::tiff_family::container::tags::SUB_IFDS,
                &sub_offsets[index],
            )
        });
        let next = top_offsets.get(index + 1).copied().unwrap_or(0);
        let tags = ifd.tags.iter().chain(sub_tag.as_ref()).collect();
        write_tree_ifd(&mut ifd_bytes, &mut data, data_start, tags, next);
        for sub in &ifd.sub_ifds {
            write_tree_ifd(
                &mut ifd_bytes,
                &mut data,
                data_start,
                sub.iter().collect(),
                0,
            );
        }
    }

    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    buf.extend_from_slice(&top_offsets.first().copied().unwrap_or(0).to_le_bytes());
    buf.extend_from_slice(&ifd_bytes);
    assert_eq!(buf.len() as u32, data_start);
    buf.extend_from_slice(&data);

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

fn write_tree_ifd(
    out: &mut Vec<u8>,
    data: &mut Vec<u8>,
    data_start: u32,
    mut tags: Vec<&SyntheticTag>,
    next: u32,
) {
    tags.sort_by_key(|tag| tag.tag);
    out.extend_from_slice(&(tags.len() as u16).to_le_bytes());
    for tag in tags {
        let mut inline_value = tag.inline_value;
        let mut ool_data = tag.ool_data.clone();
        if !tag.blobs.is_empty() {
            let mut offsets = Vec::new();
            for blob in &tag.blobs {
                offsets.extend_from_slice(&(data_start + data.len() as u32).to_le_bytes());
                data.extend_from_slice(blob);
            }
            if offsets.len() <= inline_value.len() {
                inline_value[..offsets.len()].copy_from_slice(&offsets);
            } else {
                ool_data = Some(offsets);
            }
        }
        out.extend_from_slice(&tag.tag.to_le_bytes());
        out.extend_from_slice(&tag.tiff_type.to_le_bytes());
        out.extend_from_slice(&tag.count.to_le_bytes());
        match ool_data {
            Some(bytes) => {
                out.extend_from_slice(&(data_start + data.len() as u32).to_le_bytes());
                data.extend_from_slice(&bytes);
            }
            None => out.extend_from_slice(&inline_value),
        }
    }
    out.extend_from_slice(&next.to_le_bytes());
}

/// Build a little-endian BigTIFF with chained top-level IFDs. Values of up to
/// 8 bytes sit in the entry; longer ones follow all IFDs.
pub(crate) fn build_bigtiff(ifds: &[Vec<SyntheticTag>]) -> NamedTempFile {
    const HEADER_LEN: u64 = 16;
    assert!(
        ifds.iter().flatten().all(|tag| tag.blobs.is_empty()),
        "blob offsets need build_tiff_tree"
    );
    let ifd_len = |tag_count: usize| 8 + 20 * tag_count as u64 + 8;
    let data_start = HEADER_LEN + ifds.iter().map(|tags| ifd_len(tags.len())).sum::<u64>();

    let mut buf = Vec::new();
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&43u16.to_le_bytes());
    buf.extend_from_slice(&8u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    let first_ifd = if ifds.is_empty() { 0 } else { HEADER_LEN };
    buf.extend_from_slice(&first_ifd.to_le_bytes());

    let mut data = Vec::new();
    for (index, tags) in ifds.iter().enumerate() {
        let mut sorted = tags.iter().collect::<Vec<_>>();
        sorted.sort_by_key(|tag| tag.tag);
        buf.extend_from_slice(&(sorted.len() as u64).to_le_bytes());
        for tag in sorted {
            buf.extend_from_slice(&tag.tag.to_le_bytes());
            buf.extend_from_slice(&tag.tiff_type.to_le_bytes());
            buf.extend_from_slice(&u64::from(tag.count).to_le_bytes());
            let value = tag.ool_data.as_deref().unwrap_or(&tag.inline_value);
            if value.len() <= 8 {
                let mut slot = [0; 8];
                slot[..value.len()].copy_from_slice(value);
                buf.extend_from_slice(&slot);
            } else {
                buf.extend_from_slice(&(data_start + data.len() as u64).to_le_bytes());
                data.extend_from_slice(value);
            }
        }
        let next = if index + 1 < ifds.len() {
            buf.len() as u64 + 8
        } else {
            0
        };
        buf.extend_from_slice(&next.to_le_bytes());
    }
    assert_eq!(buf.len() as u64, data_start);
    buf.extend_from_slice(&data);

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&buf).unwrap();
    file.flush().unwrap();
    file
}

pub(crate) fn build_tiff(ifds: &[Vec<SyntheticTag>]) -> NamedTempFile {
    assert!(
        ifds.iter().flatten().all(|tag| tag.blobs.is_empty()),
        "blob offsets need build_tiff_tree"
    );
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
