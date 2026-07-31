use dicom_core::{Tag, VR};
use dicom_object::file::ReadPreamble;

use super::*;

const MAX_FILE_META_BYTES: u32 = 1024 * 1024;
const MAX_FILE_META_ELEMENTS: usize = 128;
const MAX_METADATA_ELEMENT_BYTES: u32 = 16 * 1024 * 1024;
const MAX_METADATA_VALUE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_METADATA_TOKENS: usize = 2_000_000;
const MAX_METADATA_SEQUENCE_DEPTH: usize = 64;
const MAX_TRANSFER_SYNTAX_UID_BYTES: u32 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DicomPixelDataLocation {
    Native { value_offset: u64, value_len: u32 },
    Encapsulated,
    Missing,
}

pub(super) struct OpenedDicomMetadata {
    pub(super) object: DefaultDicomObject,
    pub(super) pixel_data: DicomPixelDataLocation,
    pub(super) source_identity: FileIdentity,
}

pub(super) fn open_metadata_object_until(
    path: &Path,
    stop_tag: Tag,
) -> Result<OpenedDicomMetadata, WsiError> {
    let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?;
    let pixel_data = preflight_dicom_metadata(&mut file, path)?;
    let source_identity = FileIdentity::from_open_file(path, &file)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
    let object = OpenFileOptions::new()
        .read_until(stop_tag)
        .read_preamble(ReadPreamble::Auto)
        .from_reader(file)
        .map_err(|source| invalid_slide(path, format!("cannot parse DICOM metadata: {source}")))?;
    Ok(OpenedDicomMetadata {
        object,
        pixel_data,
        source_identity,
    })
}

fn preflight_dicom_metadata(
    file: &mut File,
    path: &Path,
) -> Result<DicomPixelDataLocation, WsiError> {
    let transfer_syntax_uid = preflight_file_meta(file, path)?;
    let transfer_syntax = TransferSyntaxRegistry
        .get(&transfer_syntax_uid)
        .ok_or_else(|| {
            invalid_slide(
                path,
                format!("unsupported transfer syntax {transfer_syntax_uid}"),
            )
        })?;
    let pixel_data = {
        let mut reader =
            LazyDataSetReader::new_with_ts(&mut *file, transfer_syntax).map_err(|source| {
                invalid_slide(
                    path,
                    format!("cannot initialize DICOM metadata preflight: {source}"),
                )
            })?;
        let mut token_count = 0usize;
        let mut sequence_depth = 0usize;
        let mut declared_value_bytes = 0u64;
        let mut pixel_data = DicomPixelDataLocation::Missing;

        while let Some(token) = reader.advance() {
            token_count = token_count.saturating_add(1);
            if token_count > MAX_METADATA_TOKENS {
                return Err(invalid_slide(
                    path,
                    format!("DICOM metadata exceeds the {MAX_METADATA_TOKENS}-token limit"),
                ));
            }
            let token = token.map_err(|source| {
                invalid_slide(path, format!("cannot preflight DICOM metadata: {source}"))
            })?;
            match token {
                LazyDataToken::ElementHeader(header) => {
                    if header.tag == tags::PIXEL_DATA {
                        let value = reader
                            .advance()
                            .ok_or_else(|| invalid_slide(path, "DICOM Pixel Data has no value"))?
                            .map_err(|source| {
                                invalid_slide(
                                    path,
                                    format!("cannot preflight DICOM Pixel Data: {source}"),
                                )
                            })?;
                        pixel_data = match value {
                            LazyDataToken::LazyValue {
                                header: value_header,
                                decoder,
                            } => DicomPixelDataLocation::Native {
                                value_offset: decoder.position(),
                                value_len: value_header.len.0,
                            },
                            LazyDataToken::PixelSequenceStart => {
                                DicomPixelDataLocation::Encapsulated
                            }
                            _ => {
                                return Err(invalid_slide(
                                    path,
                                    "DICOM Pixel Data has an unsupported value encoding",
                                ));
                            }
                        };
                        break;
                    }
                    if header.len.0 > MAX_METADATA_ELEMENT_BYTES {
                        return Err(invalid_slide(
                            path,
                            format!(
                                "DICOM metadata element value limit is {MAX_METADATA_ELEMENT_BYTES} bytes, but {} declares {} bytes",
                                header.tag, header.len.0
                            ),
                        ));
                    }
                    declared_value_bytes = declared_value_bytes
                        .checked_add(u64::from(header.len.0))
                        .ok_or_else(|| invalid_slide(path, "DICOM metadata byte count overflow"))?;
                    if declared_value_bytes > MAX_METADATA_VALUE_BYTES {
                        return Err(invalid_slide(
                            path,
                            format!(
                                "DICOM metadata declares more than the {MAX_METADATA_VALUE_BYTES}-byte cumulative value limit"
                            ),
                        ));
                    }
                }
                LazyDataToken::SequenceStart { tag, .. } => {
                    if tag == tags::PIXEL_DATA {
                        pixel_data = DicomPixelDataLocation::Encapsulated;
                        break;
                    }
                    sequence_depth = sequence_depth.saturating_add(1);
                    if sequence_depth > MAX_METADATA_SEQUENCE_DEPTH {
                        return Err(invalid_slide(
                            path,
                            format!(
                                "DICOM metadata sequence nesting exceeds the {MAX_METADATA_SEQUENCE_DEPTH}-level limit"
                            ),
                        ));
                    }
                }
                LazyDataToken::PixelSequenceStart => {
                    pixel_data = DicomPixelDataLocation::Encapsulated;
                    break;
                }
                LazyDataToken::SequenceEnd => {
                    sequence_depth = sequence_depth.saturating_sub(1);
                }
                lazy @ (LazyDataToken::LazyValue { .. } | LazyDataToken::LazyItemValue { .. }) => {
                    lazy.skip().map_err(|source| {
                        invalid_slide(path, format!("cannot skip DICOM metadata value: {source}"))
                    })?;
                }
                LazyDataToken::ItemStart { .. } | LazyDataToken::ItemEnd => {}
                _ => {
                    return Err(invalid_slide(
                        path,
                        "DICOM metadata parser returned an unsupported token",
                    ));
                }
            }
        }

        if pixel_data == DicomPixelDataLocation::Missing && sequence_depth != 0 {
            return Err(invalid_slide(
                path,
                "DICOM metadata ended inside an unterminated sequence",
            ));
        }
        pixel_data
    };

    if let DicomPixelDataLocation::Native {
        value_offset,
        value_len,
    } = pixel_data
    {
        let value_end = value_offset
            .checked_add(u64::from(value_len))
            .ok_or_else(|| invalid_slide(path, "DICOM Pixel Data range overflow"))?;
        let file_len = file
            .seek(SeekFrom::End(0))
            .map_err(|source| io_error(path, source))?;
        if value_end > file_len {
            return Err(invalid_slide(
                path,
                "DICOM Pixel Data extends beyond the source file",
            ));
        }
    }
    Ok(pixel_data)
}

pub(super) fn preflight_file_meta(
    reader: &mut (impl Read + Seek),
    path: &Path,
) -> Result<String, WsiError> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|source| io_error(path, source))?;
    let mut magic = [0u8; 4];
    read_exact(reader, path, &mut magic, "DICOM magic code")?;
    if &magic != b"DICM" {
        reader
            .seek(SeekFrom::Start(128))
            .map_err(|source| io_error(path, source))?;
        read_exact(reader, path, &mut magic, "DICOM magic code")?;
    }
    if &magic != b"DICM" {
        return Err(invalid_slide(path, "missing DICOM file magic code"));
    }

    let group_length_header = read_explicit_header(reader, path)?;
    if group_length_header.tag != tags::FILE_META_INFORMATION_GROUP_LENGTH
        || group_length_header.vr != VR::UL
        || group_length_header.value_len != 4
    {
        return Err(invalid_slide(
            path,
            "invalid File Meta Information Group Length element",
        ));
    }
    let mut length_bytes = [0u8; 4];
    read_exact(reader, path, &mut length_bytes, "file meta group length")?;
    let group_length = u32::from_le_bytes(length_bytes);
    if group_length > MAX_FILE_META_BYTES {
        return Err(invalid_slide(
            path,
            format!(
                "DICOM file meta group is {group_length} bytes, exceeding the {MAX_FILE_META_BYTES}-byte limit"
            ),
        ));
    }

    let mut remaining = u64::from(group_length);
    let mut element_count = 0usize;
    let mut transfer_syntax_uid = None;
    while remaining > 0 {
        element_count = element_count.saturating_add(1);
        if element_count > MAX_FILE_META_ELEMENTS {
            return Err(invalid_slide(
                path,
                format!("DICOM file meta group exceeds the {MAX_FILE_META_ELEMENTS}-element limit"),
            ));
        }
        let header = read_explicit_header(reader, path)?;
        let encoded_len = header
            .header_len
            .checked_add(u64::from(header.value_len))
            .ok_or_else(|| invalid_slide(path, "DICOM file meta element length overflow"))?;
        if header.tag.group() != 0x0002 || encoded_len > remaining {
            return Err(invalid_slide(
                path,
                format!(
                    "DICOM file meta element {} exceeds the declared group boundary",
                    header.tag
                ),
            ));
        }
        if header.tag == tags::TRANSFER_SYNTAX_UID {
            if header.value_len == 0 || header.value_len > MAX_TRANSFER_SYNTAX_UID_BYTES {
                return Err(invalid_slide(
                    path,
                    "DICOM transfer syntax UID has an invalid declared length",
                ));
            }
            let mut value = vec![0u8; header.value_len as usize];
            read_exact(reader, path, &mut value, "DICOM transfer syntax UID")?;
            let value = String::from_utf8(value)
                .map_err(|_| invalid_slide(path, "DICOM transfer syntax UID is not UTF-8"))?;
            transfer_syntax_uid = Some(
                value
                    .trim_end_matches(|character: char| {
                        character.is_whitespace() || character == '\0'
                    })
                    .to_string(),
            );
        } else {
            reader
                .seek(SeekFrom::Current(i64::from(header.value_len)))
                .map_err(|source| io_error(path, source))?;
        }
        remaining -= encoded_len;
    }

    let dataset_offset = reader
        .stream_position()
        .map_err(|source| io_error(path, source))?;
    let file_len = reader
        .seek(SeekFrom::End(0))
        .map_err(|source| io_error(path, source))?;
    if dataset_offset > file_len {
        return Err(invalid_slide(
            path,
            "DICOM file meta group extends beyond the source file",
        ));
    }
    reader
        .seek(SeekFrom::Start(dataset_offset))
        .map_err(|source| io_error(path, source))?;

    transfer_syntax_uid
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| invalid_slide(path, "DICOM file meta group has no transfer syntax UID"))
}

struct ExplicitHeader {
    tag: Tag,
    vr: VR,
    value_len: u32,
    header_len: u64,
}

fn read_explicit_header(
    reader: &mut (impl Read + Seek),
    path: &Path,
) -> Result<ExplicitHeader, WsiError> {
    let mut base = [0u8; 8];
    read_exact(reader, path, &mut base, "DICOM file meta element header")?;
    let tag = Tag(
        u16::from_le_bytes([base[0], base[1]]),
        u16::from_le_bytes([base[2], base[3]]),
    );
    let vr = VR::from_binary([base[4], base[5]]).ok_or_else(|| {
        invalid_slide(path, format!("invalid VR in DICOM file meta element {tag}"))
    })?;
    let uses_u32_length = matches!(
        vr,
        VR::OB
            | VR::OD
            | VR::OF
            | VR::OL
            | VR::OV
            | VR::OW
            | VR::SQ
            | VR::UC
            | VR::UN
            | VR::UR
            | VR::UT
    );
    let (value_len, header_len) = if uses_u32_length {
        if base[6..8] != [0, 0] {
            return Err(invalid_slide(
                path,
                format!("invalid reserved bytes in DICOM file meta element {tag}"),
            ));
        }
        let mut length = [0u8; 4];
        read_exact(reader, path, &mut length, "DICOM file meta element length")?;
        (u32::from_le_bytes(length), 12)
    } else {
        (u32::from(u16::from_le_bytes([base[6], base[7]])), 8)
    };
    Ok(ExplicitHeader {
        tag,
        vr,
        value_len,
        header_len,
    })
}

fn read_exact(
    reader: &mut impl Read,
    path: &Path,
    bytes: &mut [u8],
    context: &str,
) -> Result<(), WsiError> {
    reader
        .read_exact(bytes)
        .map_err(|source| invalid_slide(path, format!("cannot read {context}: {source}")))
}

fn io_error(path: &Path, source: std::io::Error) -> WsiError {
    WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    }
}
