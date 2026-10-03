use super::super::error::{IfdId, TiffParseError};
use super::model::{Ifd, InlineValue, TagEntry, TagValue, TiffType};
use super::ndpi_offsets::fix_offset_ndpi;
use super::*;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

mod arrays;
mod chains;
mod fixtures;
mod headers;
mod model;
mod ndpi;
mod resolution;
mod scalars;

use crate::core::registry::OpenBudget;
use fixtures::*;

impl TiffContainer {
    /// Open and parse a TIFF or BigTIFF file.
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self, TiffParseError> {
        let budget = OpenBudget::new(crate::SlideLimits::default());
        Self::open_with_budget(path, budget)
    }
}
