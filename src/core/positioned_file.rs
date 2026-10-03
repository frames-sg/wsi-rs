//! Shared read-only file handles without cross-reader seek interference.
use std::{fs::File, io};

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
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.read_exact_at(bytes, offset)
        }
        #[cfg(not(unix))]
        {
            use std::io::{Read, Seek, SeekFrom};
            let _position = self
                .position
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut file = &self.file;
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(bytes)
        }
    }
}

#[cfg(all(test, not(unix)))]
mod tests;
