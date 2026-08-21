use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use dicom_dictionary_std::uids;
use dicom_parser::dataset::{lazy_read::LazyDataSetReader, LazyDataToken};
use dicom_parser::stateful::decode::StatefulDecode;
use dicom_transfer_syntax_registry::{TransferSyntaxIndex, TransferSyntaxRegistry};

use crate::error::WsiError;

use super::check_index_control;
use super::model::{DicomEncapsulatedFrames, DicomFragmentRef};
use super::offset_tables::{build_encapsulated_frame_index, validate_basic_offset_table_len};
use crate::formats::dicom::metadata::invalid_slide;
use crate::formats::dicom::preflight::preflight_file_meta;
use crate::formats::dicom::JP2K_TRANSFER_SYNTAXES;

pub(super) fn scan_encapsulated_frames_tokenized(
    path: &Path,
    transfer_syntax_uid: &str,
    number_of_frames: u32,
    control: Option<&crate::ReadControl>,
) -> Result<DicomEncapsulatedFrames, WsiError> {
    check_index_control(control)?;
    let transfer_syntax = TransferSyntaxRegistry
        .get(transfer_syntax_uid)
        .or_else(|| {
            JP2K_TRANSFER_SYNTAXES
                .contains(&transfer_syntax_uid)
                .then(|| TransferSyntaxRegistry.get(uids::EXPLICIT_VR_LITTLE_ENDIAN))
                .flatten()
        })
        .ok_or_else(|| {
            invalid_slide(
                path,
                format!("unknown transfer syntax {transfer_syntax_uid}"),
            )
        })?;
    let mut reader = BufReader::new(File::open(path).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: path.to_path_buf(),
    })?);
    let actual_transfer_syntax = preflight_file_meta(&mut reader, path)?;
    if actual_transfer_syntax != transfer_syntax_uid {
        return Err(invalid_slide(
            path,
            format!(
                "DICOM transfer syntax changed from {transfer_syntax_uid} to {actual_transfer_syntax}"
            ),
        ));
    }
    let mut tokens = LazyDataSetReader::new_with_ts(reader, transfer_syntax)
        .map_err(|source| invalid_slide(path, format!("cannot stream DICOM dataset: {source}")))?;

    let mut in_pixel_sequence = false;
    let mut awaiting_offset_table = false;
    let mut offset_table = Vec::new();
    let mut fragments = Vec::new();

    while let Some(token) = tokens.advance() {
        check_index_control(control)?;
        let token = token
            .map_err(|source| invalid_slide(path, format!("cannot read DICOM token: {source}")))?;
        match token {
            LazyDataToken::PixelSequenceStart => {
                in_pixel_sequence = true;
                awaiting_offset_table = true;
            }
            LazyDataToken::ItemStart { len }
                if in_pixel_sequence && awaiting_offset_table && len.0 == 0 =>
            {
                awaiting_offset_table = false;
            }
            LazyDataToken::LazyItemValue { len, decoder }
                if in_pixel_sequence && awaiting_offset_table =>
            {
                check_index_control(control)?;
                let entry_count =
                    validate_basic_offset_table_len(path, len, Some(number_of_frames))?;
                offset_table
                    .try_reserve_exact(entry_count)
                    .map_err(|_| invalid_slide(path, "cannot allocate DICOM basic offset table"))?;
                decoder
                    .read_u32_to_vec(len, &mut offset_table)
                    .map_err(|source| {
                        invalid_slide(
                            path,
                            format!("cannot read DICOM basic offset table: {source}"),
                        )
                    })?;
                awaiting_offset_table = false;
            }
            LazyDataToken::LazyItemValue { len, decoder } if in_pixel_sequence => {
                check_index_control(control)?;
                let payload_offset = decoder.position();
                let item_offset = payload_offset.saturating_sub(8);
                decoder.skip_bytes(len).map_err(|source| {
                    invalid_slide(path, format!("cannot skip DICOM fragment: {source}"))
                })?;
                fragments.push(DicomFragmentRef {
                    payload_offset,
                    item_offset,
                    len,
                });
            }
            LazyDataToken::ItemStart { len } if in_pixel_sequence && len.0 == 0 => {
                return Err(invalid_slide(
                    path,
                    "zero-length DICOM pixel fragment is not supported",
                ));
            }
            LazyDataToken::SequenceEnd if in_pixel_sequence => break,
            other => {
                other.skip().map_err(|source| {
                    invalid_slide(path, format!("cannot skip DICOM token: {source}"))
                })?;
            }
        }
    }

    check_index_control(control)?;
    build_encapsulated_frame_index(path, fragments, offset_table, number_of_frames)
}
