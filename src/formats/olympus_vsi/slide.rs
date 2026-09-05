//! VSI companion discovery and ordered public scene assembly.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::hash::{dataset_id_from_quickhash, Quickhash1};
use crate::core::registry::{BackendOpenConfig, OpenBudget};
use crate::core::types::{Dataset, Level, Scene, Series, TileLayout};
use crate::error::WsiError;
use crate::properties::Properties;

use super::{invalid_slide, scene::EtsScene};

const MAX_ETS_SCENES: usize = 1_024;

pub(super) struct OlympusVsiSlide {
    pub(super) dataset: Dataset,
    pub(super) scenes: Vec<EtsScene>,
}

impl OlympusVsiSlide {
    #[cfg(test)]
    pub(super) fn parse(path: &Path) -> Result<Self, WsiError> {
        Self::parse_with_config(path, BackendOpenConfig::deterministic())
    }

    pub(super) fn parse_with_config(
        path: &Path,
        config: BackendOpenConfig,
    ) -> Result<Self, WsiError> {
        let budget = OpenBudget::new(config.limits);
        let dir =
            companion_dir(path).ok_or_else(|| invalid_slide(path, "missing companion dir"))?;
        let mut ets_paths = find_ets_files(&dir, &budget)?;
        if ets_paths.is_empty() {
            return Err(invalid_slide(path, "no ETS frame files found"));
        }

        let scene_index_bytes = u64::try_from(ets_paths.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::try_from(std::mem::size_of::<EtsScene>()).unwrap_or(u64::MAX));
        budget.retain_index(scene_index_bytes)?;
        let mut scenes = Vec::new();
        scenes
            .try_reserve_exact(ets_paths.len())
            .map_err(|_| WsiError::ResourceLimit {
                resource: "Olympus ETS scene index",
                requested: scene_index_bytes,
                limit: config.limits.tile_index_bytes(),
            })?;
        for ets_path in ets_paths.drain(..) {
            scenes.push(EtsScene::parse_with_budget(&ets_path, &budget)?);
        }
        scenes.sort_by_key(|scene| Reverse(scene.level0_area()));

        let mut quickhash = Quickhash1::new();
        quickhash.hash_string(&path.display().to_string());
        for scene in &scenes {
            quickhash.hash_string(&scene.path.display().to_string());
            quickhash.update(&scene.path.metadata()?.len().to_le_bytes());
        }
        let quickhash = quickhash
            .finish()
            .ok_or_else(|| invalid_slide(path, "failed to compute Olympus quickhash"))?;
        let dataset_id = dataset_id_from_quickhash(path, &quickhash, "quickhash")?;

        let public_scenes = scenes
            .iter()
            .enumerate()
            .map(|(scene_index, scene)| Scene {
                id: format!("s{scene_index}"),
                name: scene.name.clone(),
                series: vec![Series {
                    id: "ser0".into(),
                    axes: scene.axes,
                    levels: scene
                        .levels
                        .iter()
                        .map(|level| Level {
                            dimensions: (level.width as u64, level.height as u64),
                            downsample: scene.levels[0].width as f64 / level.width as f64,
                            tile_layout: TileLayout::Regular {
                                tile_width: level.tile_width,
                                tile_height: level.tile_height,
                                tiles_across: level.tiles_across as u64,
                                tiles_down: level.tiles_down as u64,
                            },
                        })
                        .collect(),
                    sample_type: scene.sample_type,
                    channels: scene.channels.clone(),
                }],
            })
            .collect();

        let mut properties = Properties::new();
        properties.insert("openslide.vendor", "olympus");
        properties.insert("openslide.quickhash-1", quickhash);

        Ok(Self {
            dataset: Dataset {
                id: dataset_id,
                scenes: public_scenes,
                associated_images: HashMap::new(),
                properties,
                icc_profiles: HashMap::new(),
                source_icc_profiles: Vec::new(),
            },
            scenes,
        })
    }
}

pub(super) fn find_ets_files(dir: &Path, budget: &OpenBudget) -> Result<Vec<PathBuf>, WsiError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir).map_err(|source| WsiError::IoWithPath {
        source: Arc::new(source),
        path: dir.to_path_buf(),
    })? {
        let entry = entry?;
        let file_type = entry.file_type().map_err(|source| WsiError::IoWithPath {
            source: Arc::new(source),
            path: entry.path(),
        })?;
        let path = entry.path();
        if !file_type.is_dir() {
            continue;
        }
        let frame = path.join("frame_t.ets");
        let frame_is_regular_file = fs::symlink_metadata(&frame)
            .map(|metadata| metadata.file_type().is_file())
            .unwrap_or(false);
        if frame_is_regular_file {
            if paths.len() == MAX_ETS_SCENES {
                return Err(invalid_slide(
                    dir,
                    format!("Olympus dataset exceeds the {MAX_ETS_SCENES}-scene limit"),
                ));
            }
            let retained_bytes = u64::try_from(std::mem::size_of::<PathBuf>())
                .unwrap_or(u64::MAX)
                .saturating_add(u64::try_from(frame.as_os_str().len()).unwrap_or(u64::MAX));
            budget.retain_index(retained_bytes)?;
            paths.try_reserve(1).map_err(|_| WsiError::ResourceLimit {
                resource: "Olympus ETS bundle index",
                requested: retained_bytes,
                limit: budget.limits().tile_index_bytes(),
            })?;
            paths.push(frame);
        }
    }
    paths.sort();
    Ok(paths)
}

pub(super) fn companion_dir(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    Some(parent.join(format!("_{stem}_")))
}
