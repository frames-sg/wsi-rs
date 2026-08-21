use wsi_rs::{Properties, Slide};

#[derive(Debug)]
pub(super) struct OpenSlideGeometry {
    levels: Vec<OpenSlideLevelGeometry>,
}

#[derive(Clone, Copy, Debug)]
struct OpenSlideLevelGeometry {
    dimensions: (u64, u64),
    downsample: f64,
    scene_origin: Option<(i64, i64)>,
}

impl OpenSlideGeometry {
    pub(super) fn empty() -> Self {
        Self { levels: Vec::new() }
    }

    pub(super) fn from_slide(slide: &Slide) -> Self {
        let Some(series) = slide
            .dataset()
            .scenes
            .first()
            .and_then(|scene| scene.series.first())
        else {
            return Self::empty();
        };
        let mut geometry = Self {
            levels: series
                .levels
                .iter()
                .map(|level| OpenSlideLevelGeometry {
                    dimensions: level.dimensions,
                    downsample: level.downsample,
                    scene_origin: None,
                })
                .collect(),
        };

        if slide.dataset().properties.vendor() == Some("leica") {
            geometry.apply_leica_collection_geometry(&slide.dataset().properties);
        }
        geometry
    }

    pub(super) fn level_count(&self) -> usize {
        self.levels.len()
    }

    pub(super) fn level_dimensions(&self, level: usize) -> Option<(u64, u64)> {
        self.levels.get(level).map(|level| level.dimensions)
    }

    pub(super) fn level_downsample(&self, level: usize) -> Option<f64> {
        self.levels.get(level).map(|level| level.downsample)
    }

    pub(super) fn level_downsamples(&self) -> impl Iterator<Item = f64> + '_ {
        self.levels.iter().map(|level| level.downsample)
    }

    pub(super) fn levels(&self) -> impl Iterator<Item = ((u64, u64), f64)> + '_ {
        self.levels
            .iter()
            .map(|level| (level.dimensions, level.downsample))
    }

    pub(super) fn read_origin(
        &self,
        x: i64,
        y: i64,
        level: usize,
    ) -> Option<((i64, i64), (f64, f64))> {
        let geometry = self.levels.get(level)?;
        if geometry.downsample <= 0.0 {
            return Some(((x, y), (0.0, 0.0)));
        }
        if let Some((origin_x, origin_y)) = geometry.scene_origin {
            // OpenSlide truncates the global level coordinate and the Leica
            // physical scene origin independently before subtracting them.
            let level_x = trunc_i64(x as f64 / geometry.downsample)?;
            let level_y = trunc_i64(y as f64 / geometry.downsample)?;
            Some((
                (
                    level_x.saturating_sub(origin_x),
                    level_y.saturating_sub(origin_y),
                ),
                (0.0, 0.0),
            ))
        } else {
            let (level_x, subpixel_x) = floor_and_fraction(x as f64 / geometry.downsample)?;
            let (level_y, subpixel_y) = floor_and_fraction(y as f64 / geometry.downsample)?;
            Some(((level_x, level_y), (subpixel_x, subpixel_y)))
        }
    }

    fn apply_leica_collection_geometry(&mut self, properties: &Properties) {
        let Some(collection_width) = property_u64(properties, "leica.collection-size-x") else {
            return;
        };
        let Some(collection_height) = property_u64(properties, "leica.collection-size-y") else {
            return;
        };
        let Some(view_width) = property_u64(properties, "leica.scene[0].view-size-x") else {
            return;
        };
        let Some(offset_x) = property_i64(properties, "leica.scene[0].offset-x") else {
            return;
        };
        let Some(offset_y) = property_i64(properties, "leica.scene[0].offset-y") else {
            return;
        };
        if view_width == 0 {
            return;
        }

        let mut levels = Vec::with_capacity(self.levels.len());
        for level in &self.levels {
            let local_width = level.dimensions.0;
            if local_width == 0 {
                return;
            }
            // OpenSlide's Leica backend defines pixel size from the X-axis
            // physical view extent and uses that scalar for both canvas axes.
            let units_per_pixel = view_width as f64 / local_width as f64;
            let Some(width) = ceil_u64(collection_width as f64 / units_per_pixel) else {
                return;
            };
            let Some(height) = ceil_u64(collection_height as f64 / units_per_pixel) else {
                return;
            };
            if width == 0 || height == 0 {
                return;
            }
            let Some(scene_x) = trunc_i64(offset_x as f64 / units_per_pixel) else {
                return;
            };
            let Some(scene_y) = trunc_i64(offset_y as f64 / units_per_pixel) else {
                return;
            };
            levels.push(OpenSlideLevelGeometry {
                dimensions: (width, height),
                downsample: 1.0,
                scene_origin: Some((scene_x, scene_y)),
            });
        }

        let Some(level0) = levels.first().map(|level| level.dimensions) else {
            return;
        };
        for level in &mut levels {
            let width_ratio = level0.0 as f64 / level.dimensions.0 as f64;
            let height_ratio = level0.1 as f64 / level.dimensions.1 as f64;
            level.downsample = (width_ratio + height_ratio) / 2.0;
        }
        self.levels = levels;
    }
}

fn property_u64(properties: &Properties, name: &str) -> Option<u64> {
    properties.get(name)?.parse().ok()
}

fn property_i64(properties: &Properties, name: &str) -> Option<i64> {
    properties.get(name)?.parse().ok()
}

fn ceil_u64(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0 && value <= u64::MAX as f64).then_some(value.ceil() as u64)
}

fn trunc_i64(value: f64) -> Option<i64> {
    (value.is_finite() && value >= i64::MIN as f64 && value <= i64::MAX as f64)
        .then_some(value as i64)
}

fn floor_and_fraction(value: f64) -> Option<(i64, f64)> {
    let floor = value.floor();
    (floor.is_finite() && floor >= i64::MIN as f64 && floor <= i64::MAX as f64)
        .then_some((floor as i64, value - floor))
}
