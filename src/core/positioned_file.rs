//! Positioned reads of read-only files, and shared handles without
//! cross-reader seek interference.
use std::{fs::File, io};

/// Fills `bytes` from `offset` in `file`. On Unix the file cursor is left
/// alone; elsewhere this seeks, so readers sharing `file` across threads must
/// serialize, as `PositionedFile` does.
pub(crate) fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(bytes, offset)
    }
    #[cfg(not(unix))]
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = file;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(bytes)
    }
}

#[derive(Debug)]
pub(crate) struct PositionedFile {
    file: File,
    #[cfg(not(unix))]
    position: std::sync::Mutex<()>,
}

impl PositionedFile {
    pub(crate) fn new(file: File) -> Self {
        Self {
            file,
            #[cfg(not(unix))]
            position: std::sync::Mutex::new(()),
        }
    }

    pub(crate) fn read_exact_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<()> {
        #[cfg(not(unix))]
        let _position = self
            .position
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        read_exact_at(&self.file, bytes, offset)
    }
}

#[cfg(all(test, not(unix)))]
mod tests;
