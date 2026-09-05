//! Immutable ETS scene metadata assembled from checked headers and indexes.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::registry::OpenBudget;
use crate::core::types::{AxesShape, ChannelInfo, SampleType};
use crate::error::WsiError;

pub(super) mod header;
pub(super) mod index;

use header::EtsHeader;
use index::{EtsIndex, MAX_ETS_AXIS_INDEX};

pub(super) struct EtsScene {
    pub(super) path: PathBuf,
    pub(super) name: Option<String>,
    pub(super) levels: Vec<EtsLevel>,
    pub(super) tiles: HashMap<EtsTileKey, EtsTile>,
    pub(super) axes: AxesShape,
    pub(super) sample_type: SampleType,
    pub(super) samples_per_pixel: u32,
    pub(super) background: Vec<u8>,
    pub(super) channels: Vec<ChannelInfo>,
    pub(super) encoded_unit_limit: u64,
    pub(super) decoded_output_limit: u64,
}

impl EtsScene {
    #[cfg(test)]
    pub(super) fn parse(path: &Path) -> Result<Self, WsiError> {
        Self::parse_with_budget(path, &OpenBudget::new(crate::SlideLimits::default()))
    }

    pub(super) fn parse_with_budget(path: &Path, budget: &OpenBudget) -> Result<Self, WsiError> {
        let mut file = File::open(path).map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: path.to_path_buf(),
        })?;
        let header = EtsHeader::read(&mut file, path, budget)?;
        let index = EtsIndex::read(&mut file, path, budget, &header)?;
        let levels = index.levels(path, budget, header.tile_width, header.tile_height)?;
        let axes = index.axes(path)?;
        let channels = scene_channels(header.samples_per_pixel, axes, budget)?;
        let EtsHeader {
            sample_type,
            samples_per_pixel,
            background,
            ..
        } = header;
        let tiles = index.tiles;
        Ok(Self {
            path: path.to_path_buf(),
            name: {
                let name = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str());
                if let Some(name) = name {
                    budget.retain_metadata(u64::try_from(name.len()).unwrap_or(u64::MAX))?;
                }
                name.map(ToOwned::to_owned)
            },
            levels,
            tiles,
            axes,
            sample_type,
            samples_per_pixel,
            background,
            channels,
            encoded_unit_limit: budget.limits().encoded_unit_bytes(),
            decoded_output_limit: budget.limits().decoded_output_bytes(),
        })
    }

    pub(super) fn level0_area(&self) -> u64 {
        self.levels
            .first()
            .map(|level| level.width as u64 * level.height as u64)
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct EtsLevel {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) tile_width: u32,
    pub(super) tile_height: u32,
    pub(super) tiles_across: u32,
    pub(super) tiles_down: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) struct EtsTileKey {
    pub(super) level: u32,
    pub(super) z: u32,
    pub(super) c: u32,
    pub(super) t: u32,
    pub(super) col: u32,
    pub(super) row: u32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct EtsTile {
    pub(super) offset: u64,
    pub(super) byte_count: u32,
}

fn scene_channels(
    samples_per_pixel: u32,
    axes: AxesShape,
    budget: &OpenBudget,
) -> Result<Vec<ChannelInfo>, WsiError> {
    let channels = if samples_per_pixel == 3 {
        Vec::new()
    } else {
        let channel_name_bytes = (0..axes.c)
            .try_fold(0u64, |total, channel| {
                let digits = if channel == 0 {
                    1
                } else {
                    u64::from(channel.ilog10()) + 1
                };
                total.checked_add(8 + digits)
            })
            .unwrap_or(u64::MAX);
        let channel_bytes = u64::from(axes.c)
            .checked_mul(u64::try_from(std::mem::size_of::<ChannelInfo>()).unwrap_or(u64::MAX))
            .and_then(|bytes| bytes.checked_add(channel_name_bytes))
            .unwrap_or(u64::MAX);
        let channel_limit = u64::from(MAX_ETS_AXIS_INDEX + 1)
            .saturating_mul(u64::try_from(std::mem::size_of::<ChannelInfo>()).unwrap_or(u64::MAX))
            .min(budget.limits().aggregate_metadata_bytes());
        budget.retain_metadata(channel_bytes)?;
        let mut channels = Vec::new();
        channels
            .try_reserve_exact(axes.c as usize)
            .map_err(|_| WsiError::ResourceLimit {
                resource: "Olympus ETS channel metadata",
                requested: channel_bytes,
                limit: channel_limit,
            })?;
        channels.extend((0..axes.c).map(|c| ChannelInfo {
            name: Some(format!("Channel {c}")),
            color: None,
            excitation_nm: None,
            emission_nm: None,
        }));
        channels
    };

    Ok(channels)
}
