#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelInfo {
    pub width: u64,
    pub height: u64,
    pub downsample: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Level0Bounds {
    pub x: i64,
    pub y: i64,
    pub width: u64,
    pub height: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleSummary {
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub mean_us: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkloadPlan {
    pub levels: Vec<LevelInfo>,
    pub level0_bounds: Level0Bounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadSpec {
    pub x: i64,
    pub y: i64,
    pub level: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workload {
    pub name: &'static str,
    pub warmup: bool,
    pub reads: Vec<ReadSpec>,
}

const CACHE_PRESSURE_READ_COUNT: usize = 1_025;
const BATCH_EXPORT_READ_COUNT: usize = 1_024;
const LARGE_REGION_READ_COUNT: usize = 16;
const VIEWPORT_READ_COUNT: usize = 128;

pub const CAPTURE_WORKLOAD_NAMES: [&str; 11] = [
    "open_latency",
    "single_tile_l0",
    "pan_trace_l0",
    "pan_trace_l2",
    "viewport_region_l2",
    "zoom_trace",
    "warm_revisit_l0",
    "cache_pressure_l0",
    "thumbnail",
    "large_region_l0",
    "batch_export_l0",
];

pub fn percentile(sorted_samples: &[u64], percent: u64) -> Option<u64> {
    if sorted_samples.is_empty() || !(1..=100).contains(&percent) {
        return None;
    }
    let rank = (percent as usize)
        .saturating_mul(sorted_samples.len())
        .div_ceil(100);
    sorted_samples.get(rank.saturating_sub(1)).copied()
}

pub fn summarize_samples(samples: &[u64]) -> Option<SampleSummary> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let total = samples
        .iter()
        .fold(0u128, |sum, sample| sum + u128::from(*sample));
    let mean = total / samples.len() as u128;
    Some(SampleSummary {
        p50_us: percentile(&sorted, 50)?,
        p95_us: percentile(&sorted, 95)?,
        p99_us: percentile(&sorted, 99)?,
        mean_us: u64::try_from(mean).unwrap_or(u64::MAX),
    })
}

impl WorkloadPlan {
    pub fn with_level0_bounds(
        levels: Vec<LevelInfo>,
        level0_bounds: Level0Bounds,
    ) -> Result<Self, String> {
        if levels.is_empty() {
            return Err("slide has no levels".into());
        }
        let mut previous_downsample = 0.0;
        for (index, level) in levels.iter().enumerate() {
            if level.width == 0 || level.height == 0 {
                return Err(format!("level {index} has zero dimensions"));
            }
            if !level.downsample.is_finite() || level.downsample <= 0.0 {
                return Err(format!("level {index} has invalid downsample"));
            }
            if level.downsample < previous_downsample {
                return Err(format!("level {index} downsamples are not ordered"));
            }
            previous_downsample = level.downsample;
        }
        validate_bounds(level0_bounds)?;
        Ok(Self {
            levels,
            level0_bounds,
        })
    }

    pub fn viewer_workloads(&self) -> Vec<Workload> {
        let level0 = 0;
        let reduced = self.levels.len().saturating_sub(1).min(2);
        let coarsest = self.levels.len() - 1;

        let single = self.focused(level0, 256);
        let viewport = self.focused(reduced, 1_024);
        let thumbnail = self.centered(coarsest, 2_048);
        let revisit_points = self.revisit_points(level0, 256);

        vec![
            Workload {
                name: CAPTURE_WORKLOAD_NAMES[1],
                warmup: true,
                reads: vec![single; 128],
            },
            Workload {
                name: CAPTURE_WORKLOAD_NAMES[2],
                warmup: false,
                reads: self.pan_trace(level0, 256, 128),
            },
            Workload {
                name: CAPTURE_WORKLOAD_NAMES[3],
                warmup: false,
                reads: self.pan_trace(reduced, 256, 128),
            },
            Workload {
                name: CAPTURE_WORKLOAD_NAMES[4],
                warmup: true,
                reads: vec![viewport; VIEWPORT_READ_COUNT],
            },
            Workload {
                name: CAPTURE_WORKLOAD_NAMES[5],
                warmup: false,
                reads: (0..128)
                    .map(|index| self.focused(index % self.levels.len(), 512))
                    .collect(),
            },
            Workload {
                name: CAPTURE_WORKLOAD_NAMES[6],
                warmup: true,
                reads: (0..128)
                    .map(|index| revisit_points[index % revisit_points.len()])
                    .collect(),
            },
            Workload {
                name: CAPTURE_WORKLOAD_NAMES[7],
                warmup: false,
                reads: self.cache_pressure(level0, 256, CACHE_PRESSURE_READ_COUNT),
            },
            Workload {
                name: CAPTURE_WORKLOAD_NAMES[8],
                warmup: true,
                reads: vec![thumbnail; 30],
            },
            Workload {
                name: CAPTURE_WORKLOAD_NAMES[9],
                warmup: false,
                reads: self.pan_trace(level0, 2_048, LARGE_REGION_READ_COUNT),
            },
            Workload {
                name: CAPTURE_WORKLOAD_NAMES[10],
                warmup: false,
                reads: self.batch_export(level0, 256, BATCH_EXPORT_READ_COUNT),
            },
        ]
    }

    fn centered(&self, level_index: usize, desired_side: u32) -> ReadSpec {
        let level = self.workload_extent(level_index);
        let width = u64::from(desired_side).min(level.width) as u32;
        let height = u64::from(desired_side).min(level.height) as u32;
        let x = (level.width - u64::from(width)) / 2;
        let y = (level.height - u64::from(height)) / 2;
        self.read_spec(level_index, x, y, width, height)
    }

    fn focused(&self, level_index: usize, desired_side: u32) -> ReadSpec {
        let level = self.workload_extent(level_index);
        let width = u64::from(desired_side).min(level.width) as u32;
        let height = u64::from(desired_side).min(level.height) as u32;
        let x = (level.width - u64::from(width)) / 4;
        let y = (level.height - u64::from(height)) / 4;
        self.read_spec(level_index, x, y, width, height)
    }

    fn pan_trace(&self, level_index: usize, desired_side: u32, count: usize) -> Vec<ReadSpec> {
        let level = self.workload_extent(level_index);
        let width = u64::from(desired_side).min(level.width) as u32;
        let height = u64::from(desired_side).min(level.height) as u32;
        let x_extent = level.width - u64::from(width);
        let y_extent = level.height - u64::from(height);
        let denominator = count.saturating_sub(1).max(1) as u128;

        (0..count)
            .map(|index| {
                let position = index as u128;
                let x = (u128::from(x_extent) * position / denominator) as u64;
                let y = (u128::from(y_extent) * position / denominator) as u64;
                self.read_spec(level_index, x, y, width, height)
            })
            .collect()
    }

    fn revisit_points(&self, level_index: usize, desired_side: u32) -> [ReadSpec; 4] {
        let level = self.workload_extent(level_index);
        let width = u64::from(desired_side).min(level.width) as u32;
        let height = u64::from(desired_side).min(level.height) as u32;
        let x_extent = level.width - u64::from(width);
        let y_extent = level.height - u64::from(height);
        [
            self.read_spec(level_index, x_extent / 4, y_extent / 4, width, height),
            self.read_spec(
                level_index,
                x_extent.saturating_mul(3) / 4,
                y_extent / 4,
                width,
                height,
            ),
            self.read_spec(
                level_index,
                x_extent / 4,
                y_extent.saturating_mul(3) / 4,
                width,
                height,
            ),
            self.read_spec(
                level_index,
                x_extent.saturating_mul(3) / 4,
                y_extent.saturating_mul(3) / 4,
                width,
                height,
            ),
        ]
    }

    fn cache_pressure(&self, level_index: usize, desired_side: u32, count: usize) -> Vec<ReadSpec> {
        let level = self.workload_extent(level_index);
        let width = u64::from(desired_side).min(level.width) as u32;
        let height = u64::from(desired_side).min(level.height) as u32;
        let columns = (level.width - u64::from(width)) / u64::from(width) + 1;
        let rows = (level.height - u64::from(height)) / u64::from(height) + 1;
        let unique_slots = columns.saturating_mul(rows).max(1);

        (0..count)
            .map(|index| {
                let slot = index as u64 % unique_slots;
                let x = (slot % columns).saturating_mul(u64::from(width));
                let y = (slot / columns).saturating_mul(u64::from(height));
                self.read_spec(level_index, x, y, width, height)
            })
            .collect()
    }

    fn batch_export(&self, level_index: usize, desired_side: u32, count: usize) -> Vec<ReadSpec> {
        let level = self.workload_extent(level_index);
        let tile_width = u64::from(desired_side).min(level.width);
        let tile_height = u64::from(desired_side).min(level.height);
        let columns = level.width.div_ceil(tile_width);
        let rows = level.height.div_ceil(tile_height);
        let read_count = u64::try_from(count)
            .unwrap_or(u64::MAX)
            .min(columns.saturating_mul(rows));

        (0..read_count)
            .map(|slot| {
                let x = (slot % columns).saturating_mul(tile_width);
                let y = (slot / columns).saturating_mul(tile_height);
                let width = tile_width.min(level.width - x) as u32;
                let height = tile_height.min(level.height - y) as u32;
                self.read_spec(level_index, x, y, width, height)
            })
            .collect()
    }

    fn read_spec(
        &self,
        level_index: usize,
        x_level: u64,
        y_level: u64,
        width: u32,
        height: u32,
    ) -> ReadSpec {
        let downsample = self.levels[level_index].downsample;
        ReadSpec {
            x: add_scaled_offset(self.level0_bounds.x, x_level, downsample),
            y: add_scaled_offset(self.level0_bounds.y, y_level, downsample),
            level: level_index as u32,
            width,
            height,
        }
    }

    fn workload_extent(&self, level_index: usize) -> LevelInfo {
        let level = self.levels[level_index];
        LevelInfo {
            width: bounded_level_length(self.level0_bounds.width, level.downsample)
                .min(level.width),
            height: bounded_level_length(self.level0_bounds.height, level.downsample)
                .min(level.height),
            downsample: level.downsample,
        }
    }
}

fn validate_bounds(bounds: Level0Bounds) -> Result<(), String> {
    if bounds.width == 0 || bounds.height == 0 {
        return Err("level-0 tissue bounds have zero dimensions".into());
    }
    let width = i64::try_from(bounds.width)
        .map_err(|_| "level-0 tissue bounds width exceeds i64".to_string())?;
    let height = i64::try_from(bounds.height)
        .map_err(|_| "level-0 tissue bounds height exceeds i64".to_string())?;
    bounds
        .x
        .checked_add(width)
        .ok_or_else(|| "level-0 tissue bounds x extent overflows i64".to_string())?;
    bounds
        .y
        .checked_add(height)
        .ok_or_else(|| "level-0 tissue bounds y extent overflows i64".to_string())?;
    Ok(())
}

fn bounded_level_length(level0_length: u64, downsample: f64) -> u64 {
    ((level0_length as f64 / downsample).floor() as u64).max(1)
}

fn add_scaled_offset(origin: i64, coordinate: u64, downsample: f64) -> i64 {
    origin
        .checked_add(scale_coordinate(coordinate, downsample))
        .expect("validated tissue-bound workload coordinate must fit i64")
}

fn scale_coordinate(coordinate: u64, downsample: f64) -> i64 {
    let scaled = coordinate as f64 * downsample;
    if scaled >= i64::MAX as f64 {
        i64::MAX
    } else {
        scaled.round() as i64
    }
}

#[cfg(test)]
#[path = "tests/workload.rs"]
mod tests;
