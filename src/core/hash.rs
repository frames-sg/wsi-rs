use crate::core::types::DatasetId;
use crate::error::WsiError;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::ErrorKind;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub(crate) fn dataset_id_from_quickhash(
    path: &Path,
    quickhash: &str,
    hash_label: &str,
) -> Result<DatasetId, WsiError> {
    if quickhash.len() < 32 {
        return Err(WsiError::InvalidSlide {
            path: path.to_path_buf(),
            message: format!("{hash_label} too short"),
        });
    }
    let value = u128::from_str_radix(&quickhash[..32], 16).map_err(|_| WsiError::InvalidSlide {
        path: path.to_path_buf(),
        message: format!("{hash_label} is not valid hex"),
    })?;
    Ok(DatasetId::new(value))
}

#[derive(Clone)]
pub(crate) struct Quickhash1 {
    hasher: Sha256,
    enabled: bool,
}

impl Quickhash1 {
    pub(crate) fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            enabled: true,
        }
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        if self.enabled && !data.is_empty() {
            self.hasher.update(data);
        }
    }

    /// Hash string including a null terminator for compatibility hashing.
    pub(crate) fn hash_string(&mut self, s: &str) {
        if self.enabled {
            self.hasher.update(s.as_bytes());
            self.hasher.update([0u8]);
        }
    }

    /// Hash `size` bytes from `path` starting at `offset`. None = to end of file.
    pub(crate) fn hash_file_part(
        &mut self,
        path: &Path,
        offset: u64,
        size: Option<u64>,
    ) -> Result<(), WsiError> {
        if !self.enabled {
            return Ok(());
        }
        let mut f = File::open(path)?;
        let file_len = f.metadata()?.len();
        if offset > file_len {
            return Err(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                format!("offset {offset} exceeds file length {file_len}"),
            )
            .into());
        }
        let available = file_len - offset;
        let actual_size = match size {
            Some(s) if s > available => {
                return Err(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    format!(
                        "requested {s} bytes at offset {offset}, but only {available} bytes remain"
                    ),
                )
                .into());
            }
            Some(s) => s,
            None => available,
        };
        if offset > 0 {
            f.seek(SeekFrom::Start(offset))?;
        }
        let mut remaining = actual_size;
        let mut buf = [0u8; 4096];
        while remaining > 0 {
            let to_read = (remaining as usize).min(buf.len());
            let n = f.read(&mut buf[..to_read])?;
            if n == 0 {
                break;
            }
            self.hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
        Ok(())
    }

    pub(crate) fn hash_file(&mut self, path: &Path) -> Result<(), WsiError> {
        self.hash_file_part(path, 0, None)
    }

    pub(crate) fn disable(&mut self) {
        self.enabled = false;
    }

    pub(crate) fn finish(self) -> Option<String> {
        if self.enabled {
            Some(format!("{:x}", self.hasher.finalize()))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests;
