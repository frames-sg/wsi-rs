mod access;
mod model;
mod ndpi_offsets;
mod parse;

#[cfg(test)]
mod tests;

// Keep the facade re-exports stable for crate-internal TIFF callers and tests.
#[allow(unused_imports)]
pub(crate) use model::{
    tags, Endian, Ifd, InlineValue, TagEntry, TagValue, TiffContainer, TiffType,
};
