use super::*;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PixelComparison {
    pub reference_version: String,
    pub workloads: Vec<WorkloadPixelComparison>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkloadPixelComparison {
    pub name: String,
    pub candidate_checksum_sha256: String,
    pub reference_checksum_sha256: String,
    pub regions: u64,
    pub max_abs: u8,
    pub max_mean_abs: f64,
    pub alpha_exact: bool,
}

/// Compare the benchmark's actual read requests in a separate process, outside
/// all latency and peak-RSS measurements. Digests bind these numerical results
/// to the pixels recorded by the timed workers.
pub fn compare_pixels(config: &WorkerConfig) -> Result<PixelComparison, String> {
    let reference_path = config
        .comparison_library
        .as_deref()
        .ok_or("pixel comparison requires --compare-library")?;
    let candidate_api = OpenSlideApi::load(&config.library_path)?;
    let reference_api = OpenSlideApi::load(reference_path)?;
    let reference_version = reference_api.version()?;
    if reference_version != "4.0.1" {
        return Err(format!(
            "pixel comparison requires OpenSlide 4.0.1, found {reference_version}"
        ));
    }
    let candidate_cache = candidate_api.create_cache(config.cache_bytes)?;
    let reference_cache = reference_api.create_cache(config.cache_bytes)?;
    let candidate = candidate_api.open_with_cache(&config.slide_path, &candidate_cache)?;
    let reference = reference_api.open_with_cache(&config.slide_path, &reference_cache)?;
    let levels = candidate
        .levels()?
        .into_iter()
        .map(LevelInfo::from)
        .collect::<Vec<_>>();
    let bounds = candidate
        .level0_bounds()?
        .map(Level0Bounds::from)
        .unwrap_or_else(|| full_level0_bounds(&levels));
    let reference_levels = reference
        .levels()?
        .into_iter()
        .map(LevelInfo::from)
        .collect::<Vec<_>>();
    let reference_bounds = reference
        .level0_bounds()?
        .map(Level0Bounds::from)
        .unwrap_or_else(|| full_level0_bounds(&reference_levels));
    if levels != reference_levels || bounds != reference_bounds {
        return Err("pixel comparison requires matching slide geometry".into());
    }
    let plan = WorkloadPlan::with_level0_bounds(levels, bounds)?;
    let mut workloads = Vec::new();
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for workload in plan.viewer_workloads() {
        if config
            .only
            .as_deref()
            .is_some_and(|name| name != workload.name)
        {
            continue;
        }
        let mut result = WorkloadPixelComparison {
            name: workload.name.into(),
            candidate_checksum_sha256: String::new(),
            reference_checksum_sha256: String::new(),
            regions: 0,
            max_abs: 0,
            max_mean_abs: 0.0,
            alpha_exact: true,
        };
        let mut candidate_digest = Sha256::new();
        let mut reference_digest = Sha256::new();
        for spec in workload.reads {
            candidate.read_region_argb_into(
                spec.x,
                spec.y,
                spec.level,
                spec.width,
                spec.height,
                &mut actual,
            )?;
            reference.read_region_argb_into(
                spec.x,
                spec.y,
                spec.level,
                spec.width,
                spec.height,
                &mut expected,
            )?;
            if actual.len() != expected.len() || actual.is_empty() {
                return Err("pixel comparison returned empty or unequal region buffers".into());
            }
            let (max_abs, mean_abs, alpha_exact) = pixel_difference(&actual, &expected);
            result.max_abs = result.max_abs.max(max_abs);
            result.max_mean_abs = result.max_mean_abs.max(mean_abs);
            result.alpha_exact &= alpha_exact;
            result.regions += 1;
            candidate_digest.update(read_digest(spec, &actual));
            reference_digest.update(read_digest(spec, &expected));
        }
        result.candidate_checksum_sha256 = format!("{:x}", candidate_digest.finalize());
        result.reference_checksum_sha256 = format!("{:x}", reference_digest.finalize());
        workloads.push(result);
    }
    Ok(PixelComparison {
        reference_version,
        workloads,
    })
}

fn pixel_difference(actual: &[u32], expected: &[u32]) -> (u8, f64, bool) {
    let mut max_abs = 0;
    let mut sum_abs = 0u64;
    let mut alpha_exact = true;
    for (&actual, &expected) in actual.iter().zip(expected) {
        alpha_exact &= actual >> 24 == expected >> 24;
        for shift in [0, 8, 16] {
            let difference = ((actual >> shift) as u8).abs_diff((expected >> shift) as u8);
            max_abs = max_abs.max(difference);
            sum_abs += u64::from(difference);
        }
    }
    (
        max_abs,
        sum_abs as f64 / (actual.len() * 3) as f64,
        alpha_exact,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numerical_difference_keeps_alpha_exact_and_out_of_the_rgb_mean() {
        let (max, mean, alpha) = pixel_difference(&[0xff03_0000], &[0xff00_0000]);
        assert_eq!(max, 3);
        assert_eq!(mean, 1.0);
        assert!(alpha);
        assert_eq!(
            pixel_difference(&[0xfe00_0000], &[0xff00_0000]),
            (0, 0.0, false)
        );
    }
}
