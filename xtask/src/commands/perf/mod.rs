mod acceptance;
mod capture;
mod checksum;
mod comparison;
mod manifest;
mod metadata;
mod process_metrics;
mod profile;
mod schema;
mod worker;

pub(super) use capture::{capture, capture_openslide, capture_pair};
pub(super) use comparison::compare;
pub(super) use profile::profile;

pub(super) const PERF_CAPTURE_SCHEMA_VERSION: u32 = 6;
