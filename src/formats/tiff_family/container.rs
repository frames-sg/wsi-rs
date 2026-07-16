mod access;
mod model;
mod ndpi_offsets;
mod parse;

#[cfg(test)]
mod tests;

pub(crate) use model::{tags, Endian, TiffContainer};

#[cfg(test)]
use model::{Ifd, InlineValue, TagEntry, TagValue, TiffType};
