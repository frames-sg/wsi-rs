use std::ops::Range;

#[derive(Clone, Copy, Debug)]
pub(in super::super) struct DicomFragmentRef {
    pub(in super::super) payload_offset: u64,
    pub(in super::super) item_offset: u64,
    pub(in super::super) len: u32,
}

#[derive(Debug)]
pub(in super::super) struct DicomEncapsulatedFrames {
    pub(in super::super) fragments: Vec<DicomFragmentRef>,
    pub(in super::super) frame_ranges: Vec<Range<usize>>,
}

#[derive(Debug)]
pub(in super::super) struct DicomExtendedOffsetTables {
    pub(in super::super) offsets: Vec<u64>,
    pub(in super::super) lengths: Vec<u64>,
}

pub(super) struct FastDicomFrameIndex {
    pub(super) frames: DicomEncapsulatedFrames,
    pub(super) mapping: crate::DicomIndexMapping,
}
