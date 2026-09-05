#[cfg(test)]
use super::slide::ZEISS_DIRECT_LEVEL_COMPOSE_HITS;
use super::*;

impl ZeissSlide {
    pub(super) fn scene_level_image(
        &self,
        scene: usize,
        level: usize,
    ) -> Result<Arc<CpuTile>, WsiError> {
        if let Some(cached) = self
            .level_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(scene, level))
            .cloned()
        {
            return Ok(cached);
        }

        let buffer = self
            .scene_level_image_from_subblocks(scene, level)?
            .ok_or_else(|| {
                WsiError::UnsupportedFormat(format!(
                    "Zeiss level {level} requires direct subblock composition"
                ))
            })?;
        #[cfg(test)]
        ZEISS_DIRECT_LEVEL_COMPOSE_HITS.fetch_add(1, Ordering::Relaxed);
        let arc = Arc::new(buffer);
        let retained_bytes = u64::try_from(arc.data.byte_size()).unwrap_or(u64::MAX);
        self.level_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put((scene, level), arc.clone(), retained_bytes);
        Ok(arc)
    }

    fn scene_level_image_from_subblocks(
        &self,
        scene: usize,
        level: usize,
    ) -> Result<Option<CpuTile>, WsiError> {
        let candidate_indices = self
            .canvas_level_subblocks
            .get(level)
            .cloned()
            .unwrap_or_default();
        if candidate_indices.is_empty() {
            return Ok(None);
        }

        let candidate_infos = {
            let czi = self.czi.lock().unwrap_or_else(|e| e.into_inner());
            let all = czi.subblocks();
            let mut selected = Vec::with_capacity(candidate_indices.len());
            for index in candidate_indices {
                let info = all.get(index).cloned().ok_or_else(|| {
                    WsiError::DisplayConversion(format!(
                        "Zeiss subblock index {index} out of range"
                    ))
                })?;
                if !matches!(
                    info.compression,
                    CziCompressionMode::UnCompressed
                        | CziCompressionMode::Jpg
                        | CziCompressionMode::JpgXr
                ) {
                    return Ok(None);
                }
                selected.push(info);
            }
            selected
        };

        if candidate_infos.is_empty() {
            return Ok(None);
        }

        let series = &self.dataset.scenes[scene].series[0];
        let level_ref = &series.levels[level];

        let subblocks = candidate_infos;

        self.compose_subblocks(
            &subblocks,
            (level_ref.dimensions.0 as u32, level_ref.dimensions.1 as u32),
            (0, 0),
            level_ref.downsample.round().max(1.0) as i32,
        )
        .map(Some)
    }
}
