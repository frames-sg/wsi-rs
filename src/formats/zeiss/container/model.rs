use std::collections::{BTreeMap, HashMap};

use super::pixel::{CompressionMode, PixelType};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::formats::zeiss) enum Dimension {
    Z,
    C,
    T,
    R,
    S,
    I,
    H,
    V,
    B,
    X,
    Y,
    M,
}

impl Dimension {
    pub(in crate::formats::zeiss) const FRAME_ORDER: [Self; 9] = [
        Self::S,
        Self::T,
        Self::C,
        Self::Z,
        Self::R,
        Self::I,
        Self::H,
        Self::V,
        Self::B,
    ];

    pub(in crate::formats::zeiss) fn as_str(self) -> &'static str {
        match self {
            Self::Z => "Z",
            Self::C => "C",
            Self::T => "T",
            Self::R => "R",
            Self::S => "S",
            Self::I => "I",
            Self::H => "H",
            Self::V => "V",
            Self::B => "B",
            Self::X => "X",
            Self::Y => "Y",
            Self::M => "M",
        }
    }

    pub(in crate::formats::zeiss) fn from_code(code: &str) -> Option<Self> {
        match code.trim().to_ascii_uppercase().as_str() {
            "Z" => Some(Self::Z),
            "C" => Some(Self::C),
            "T" => Some(Self::T),
            "R" => Some(Self::R),
            "S" => Some(Self::S),
            "I" => Some(Self::I),
            "H" => Some(Self::H),
            "V" => Some(Self::V),
            "B" => Some(Self::B),
            "X" => Some(Self::X),
            "Y" => Some(Self::Y),
            "M" => Some(Self::M),
            _ => None,
        }
    }

    pub(in crate::formats::zeiss) fn is_frame_dimension(self) -> bool {
        matches!(
            self,
            Self::Z | Self::C | Self::T | Self::R | Self::S | Self::I | Self::H | Self::V | Self::B
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct Interval {
    pub(in crate::formats::zeiss) start: i32,
    pub(in crate::formats::zeiss) size: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct DimBounds {
    intervals: BTreeMap<Dimension, Interval>,
}

impl DimBounds {
    pub(in crate::formats::zeiss) fn get(&self, dimension: Dimension) -> Option<Interval> {
        self.intervals.get(&dimension).copied()
    }

    pub(in crate::formats::zeiss) fn set(&mut self, dimension: Dimension, interval: Interval) {
        self.intervals.insert(dimension, interval);
    }

    pub(in crate::formats::zeiss) fn update_value(&mut self, dimension: Dimension, value: i32) {
        match self.intervals.get_mut(&dimension) {
            Some(existing) => {
                let start = i64::from(existing.start);
                let end = start + i64::try_from(existing.size).unwrap_or(i64::MAX);
                let value = i64::from(value);
                let new_start = start.min(value);
                let new_end = end.max(value + 1);
                existing.start = i32::try_from(new_start).unwrap_or(i32::MIN);
                existing.size = usize::try_from(new_end - new_start).unwrap_or(usize::MAX);
            }
            None => {
                self.set(
                    dimension,
                    Interval {
                        start: value,
                        size: 1,
                    },
                );
            }
        }
    }

    pub(in crate::formats::zeiss) fn iter(
        &self,
    ) -> impl Iterator<Item = (Dimension, Interval)> + '_ {
        self.intervals
            .iter()
            .map(|(dimension, interval)| (*dimension, *interval))
    }

    #[allow(dead_code)]
    pub(in crate::formats::zeiss) fn to_size_map(&self) -> HashMap<String, usize> {
        self.iter()
            .map(|(dimension, interval)| (dimension.as_str().to_owned(), interval.size))
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct Coordinate {
    values: BTreeMap<Dimension, i32>,
}

impl Coordinate {
    pub(in crate::formats::zeiss) fn new() -> Self {
        Self::default()
    }

    pub(in crate::formats::zeiss) fn set(&mut self, dimension: Dimension, value: i32) {
        self.values.insert(dimension, value);
    }

    pub(in crate::formats::zeiss) fn get(&self, dimension: Dimension) -> Option<i32> {
        self.values.get(&dimension).copied()
    }

    pub(in crate::formats::zeiss) fn iter(&self) -> impl Iterator<Item = (Dimension, i32)> + '_ {
        self.values
            .iter()
            .map(|(dimension, value)| (*dimension, *value))
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct IntRect {
    pub(in crate::formats::zeiss) x: i32,
    pub(in crate::formats::zeiss) y: i32,
    pub(in crate::formats::zeiss) w: i32,
    pub(in crate::formats::zeiss) h: i32,
}

impl IntRect {
    pub(in crate::formats::zeiss) fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub(in crate::formats::zeiss) fn is_valid(self) -> bool {
        self.w >= 0 && self.h >= 0
    }

    pub(in crate::formats::zeiss) fn intersect(self, other: Self) -> Option<Self> {
        let x1 = i64::from(self.x.max(other.x));
        let y1 = i64::from(self.y.max(other.y));
        let x2 =
            (i64::from(self.x) + i64::from(self.w)).min(i64::from(other.x) + i64::from(other.w));
        let y2 =
            (i64::from(self.y) + i64::from(self.h)).min(i64::from(other.y) + i64::from(other.h));
        if x2 <= x1 || y2 <= y1 {
            return None;
        }
        Some(Self::new(
            i32::try_from(x1).ok()?,
            i32::try_from(y1).ok()?,
            i32::try_from(x2 - x1).ok()?,
            i32::try_from(y2 - y1).ok()?,
        ))
    }

    pub(in crate::formats::zeiss) fn checked_union(self, other: Self) -> Option<Self> {
        if !self.is_valid() {
            return other.is_valid().then_some(other);
        }
        if !other.is_valid() {
            return Some(self);
        }
        let x1 = i64::from(self.x.min(other.x));
        let y1 = i64::from(self.y.min(other.y));
        let x2 =
            (i64::from(self.x) + i64::from(self.w)).max(i64::from(other.x) + i64::from(other.w));
        let y2 =
            (i64::from(self.y) + i64::from(self.h)).max(i64::from(other.y) + i64::from(other.h));
        Some(Self::new(
            i32::try_from(x1).ok()?,
            i32::try_from(y1).ok()?,
            i32::try_from(x2 - x1).ok()?,
            i32::try_from(y2 - y1).ok()?,
        ))
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct IntSize {
    pub(in crate::formats::zeiss) w: u32,
    pub(in crate::formats::zeiss) h: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) enum SubBlockPyramidType {
    None,
    SingleSubBlock,
    MultiSubBlock,
}

impl SubBlockPyramidType {
    pub(in crate::formats::zeiss) fn from_raw(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::SingleSubBlock),
            2 => Some(Self::MultiSubBlock),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct FileHeaderInfo {
    pub(in crate::formats::zeiss) major: i32,
    pub(in crate::formats::zeiss) minor: i32,
    pub(in crate::formats::zeiss) primary_file_guid: String,
    pub(in crate::formats::zeiss) file_guid: String,
    pub(in crate::formats::zeiss) file_part: i32,
    pub(in crate::formats::zeiss) subblock_directory_position: u64,
    pub(in crate::formats::zeiss) metadata_position: u64,
    pub(in crate::formats::zeiss) attachment_directory_position: u64,
    pub(in crate::formats::zeiss) update_pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct DirectorySubBlockInfo {
    pub(in crate::formats::zeiss) index: usize,
    pub(in crate::formats::zeiss) file_position: u64,
    pub(in crate::formats::zeiss) file_part: i32,
    pub(in crate::formats::zeiss) pixel_type: PixelType,
    pub(in crate::formats::zeiss) compression: CompressionMode,
    pub(in crate::formats::zeiss) coordinate: Coordinate,
    pub(in crate::formats::zeiss) rect: IntRect,
    pub(in crate::formats::zeiss) stored_size: IntSize,
    pub(in crate::formats::zeiss) m_index: Option<i32>,
    pub(in crate::formats::zeiss) pyramid_type: Option<SubBlockPyramidType>,
}

impl DirectorySubBlockInfo {
    pub(in crate::formats::zeiss) fn is_layer0(&self) -> bool {
        self.rect.w >= 0
            && self.rect.h >= 0
            && self.stored_size.w == self.rect.w as u32
            && self.stored_size.h == self.rect.h as u32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct AttachmentInfo {
    pub(in crate::formats::zeiss) index: usize,
    pub(in crate::formats::zeiss) file_position: u64,
    pub(in crate::formats::zeiss) file_part: i32,
    pub(in crate::formats::zeiss) content_guid: String,
    pub(in crate::formats::zeiss) content_file_type: String,
    pub(in crate::formats::zeiss) name: String,
    pub(in crate::formats::zeiss) data_size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct AttachmentBlob {
    pub(in crate::formats::zeiss) info: AttachmentInfo,
    pub(in crate::formats::zeiss) data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct RawSubBlock {
    pub(in crate::formats::zeiss) info: DirectorySubBlockInfo,
    pub(in crate::formats::zeiss) metadata: Vec<u8>,
    pub(in crate::formats::zeiss) data: Vec<u8>,
    pub(in crate::formats::zeiss) attachment: Vec<u8>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct BoundingBoxes {
    pub(in crate::formats::zeiss) all: IntRect,
    pub(in crate::formats::zeiss) layer0: IntRect,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct SubBlockStatistics {
    pub(in crate::formats::zeiss) subblock_count: usize,
    pub(in crate::formats::zeiss) dim_bounds: DimBounds,
    pub(in crate::formats::zeiss) bounding_box: Option<IntRect>,
    pub(in crate::formats::zeiss) bounding_box_layer0: Option<IntRect>,
    pub(in crate::formats::zeiss) scene_bounding_boxes: BTreeMap<i32, BoundingBoxes>,
    pub(in crate::formats::zeiss) min_m_index: Option<i32>,
    pub(in crate::formats::zeiss) max_m_index: Option<i32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::formats::zeiss) struct ScalingInfo {
    pub(in crate::formats::zeiss) x: Option<f64>,
    pub(in crate::formats::zeiss) y: Option<f64>,
    pub(in crate::formats::zeiss) z: Option<f64>,
    pub(in crate::formats::zeiss) unit: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct MetadataChannelInfo {
    pub(in crate::formats::zeiss) index: usize,
    pub(in crate::formats::zeiss) id: Option<String>,
    pub(in crate::formats::zeiss) name: Option<String>,
    pub(in crate::formats::zeiss) pixel_type: Option<PixelType>,
    pub(in crate::formats::zeiss) color: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct DocumentInfo {
    pub(in crate::formats::zeiss) name: Option<String>,
    pub(in crate::formats::zeiss) title: Option<String>,
    pub(in crate::formats::zeiss) comment: Option<String>,
    pub(in crate::formats::zeiss) author: Option<String>,
    pub(in crate::formats::zeiss) user_name: Option<String>,
    pub(in crate::formats::zeiss) creation_date: Option<String>,
    pub(in crate::formats::zeiss) description: Option<String>,
    pub(in crate::formats::zeiss) application_name: Option<String>,
    pub(in crate::formats::zeiss) application_version: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::formats::zeiss) struct ImageInfo {
    pub(in crate::formats::zeiss) pixel_type: Option<PixelType>,
    pub(in crate::formats::zeiss) sizes: BTreeMap<Dimension, usize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::formats::zeiss) struct MetadataSummary {
    pub(in crate::formats::zeiss) document: DocumentInfo,
    pub(in crate::formats::zeiss) image: ImageInfo,
    pub(in crate::formats::zeiss) scaling: ScalingInfo,
    pub(in crate::formats::zeiss) channels: Vec<MetadataChannelInfo>,
}
