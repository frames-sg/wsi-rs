mod directory;
mod format;
mod metadata;
mod model;
mod parser;
mod payload;
mod pixel;

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use crate::error::WsiError;

pub(super) use model::*;
pub(super) use pixel::*;

struct ParsedCziContainer {
    header: FileHeaderInfo,
    subblocks: Vec<DirectorySubBlockInfo>,
    attachments: Vec<AttachmentInfo>,
    statistics: SubBlockStatistics,
}

#[derive(Debug)]
pub(super) struct CziContainer {
    reader: BufReader<File>,
    file_len: u64,
    header: FileHeaderInfo,
    subblocks: Vec<DirectorySubBlockInfo>,
    attachments: Vec<AttachmentInfo>,
    statistics: SubBlockStatistics,
    metadata_xml: Option<String>,
    metadata: Option<MetadataSummary>,
}

impl CziContainer {
    pub(super) fn open(path: impl AsRef<Path>) -> Result<Self, WsiError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.clone(),
        })?;
        let file_len = file
            .metadata()
            .map_err(|source| WsiError::IoWithPath {
                source: Arc::new(source),
                path: path.clone(),
            })?
            .len();
        let mut reader = BufReader::new(file);
        let parsed = parser::parse_file(&mut reader, file_len)?;
        Ok(Self {
            reader,
            file_len,
            header: parsed.header,
            subblocks: parsed.subblocks,
            attachments: parsed.attachments,
            statistics: parsed.statistics,
            metadata_xml: None,
            metadata: None,
        })
    }

    pub(super) fn file_header(&self) -> &FileHeaderInfo {
        &self.header
    }

    pub(super) fn statistics(&self) -> &SubBlockStatistics {
        &self.statistics
    }

    pub(super) fn subblocks(&self) -> &[DirectorySubBlockInfo] {
        &self.subblocks
    }

    pub(super) fn attachments(&self) -> &[AttachmentInfo] {
        &self.attachments
    }

    pub(super) fn metadata_xml(&mut self) -> Result<&str, WsiError> {
        if self.metadata_xml.is_none() {
            self.metadata_xml = Some(payload::read_metadata_xml(
                &mut self.reader,
                self.file_len,
                self.header.metadata_position,
            )?);
        }
        Ok(self.metadata_xml.as_deref().unwrap_or_default())
    }

    pub(super) fn metadata(&mut self) -> Result<&MetadataSummary, WsiError> {
        if self.metadata.is_none() {
            let xml = self.metadata_xml()?.to_owned();
            self.metadata = Some(metadata::parse_metadata_xml(&xml)?);
        }
        Ok(self.metadata.as_ref().expect("metadata initialized"))
    }

    pub(super) fn read_subblock(&mut self, index: usize) -> Result<RawSubBlock, WsiError> {
        let info = self.subblocks.get(index).cloned().ok_or_else(|| {
            WsiError::DisplayConversion(format!(
                "CZI subblock index {index} exceeds count {}",
                self.subblocks.len()
            ))
        })?;
        payload::read_raw_subblock(&mut self.reader, self.file_len, &info)
    }

    pub(super) fn read_attachment(&mut self, index: usize) -> Result<AttachmentBlob, WsiError> {
        let info = self.attachments.get(index).cloned().ok_or_else(|| {
            WsiError::DisplayConversion(format!(
                "CZI attachment index {index} exceeds count {}",
                self.attachments.len()
            ))
        })?;
        payload::read_attachment_blob(&mut self.reader, self.file_len, &info)
    }
}
