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
    let workloads = compare_read_plan(&plan, config.only.as_deref(), |spec, actual, expected| {
        candidate.read_region_argb_into(
            spec.x,
            spec.y,
            spec.level,
            spec.width,
            spec.height,
            actual,
        )?;
        reference.read_region_argb_into(
            spec.x,
            spec.y,
            spec.level,
            spec.width,
            spec.height,
            expected,
        )
    })?;
    Ok(PixelComparison {
        reference_version,
        workloads,
    })
}

fn compare_read_plan(
    plan: &WorkloadPlan,
    only: Option<&str>,
    mut read: impl FnMut(ReadSpec, &mut Vec<u32>, &mut Vec<u32>) -> Result<(), String>,
) -> Result<Vec<WorkloadPixelComparison>, String> {
    let mut workloads = Vec::new();
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for workload in plan.viewer_workloads() {
        if only.is_some_and(|name| name != workload.name) {
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
            read(spec, &mut actual, &mut expected)?;
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
    Ok(workloads)
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
    fn comparison_reports_every_region_and_binds_the_timed_pixels() {
        let plan = WorkloadPlan::with_level0_bounds(
            vec![LevelInfo {
                width: 4,
                height: 2,
                downsample: 1.0,
            }],
            Level0Bounds {
                x: 0,
                y: 0,
                width: 4,
                height: 2,
            },
        )
        .unwrap();
        let mut read_count = 0usize;
        let reports = compare_read_plan(&plan, None, |spec, actual, expected| {
            assert_eq!((spec.width, spec.height), (4, 2));
            expected.clear();
            expected.resize(8, 0xff03_0201);
            actual.clone_from(expected);
            actual[0] ^= 0x0100_0001;
            read_count += 1;
            Ok(())
        })
        .unwrap();
        let workloads = plan.viewer_workloads();
        assert_eq!(reports.len(), workloads.len());
        assert_eq!(
            read_count,
            workloads.iter().map(|w| w.reads.len()).sum::<usize>()
        );
        for (report, workload) in reports.iter().zip(workloads) {
            assert_eq!(report.name, workload.name);
            assert_eq!(report.regions as usize, workload.reads.len());
            assert_eq!(report.max_abs, 1);
            assert_eq!(report.max_mean_abs, 1.0 / 24.0);
            assert!(!report.alpha_exact);
            let mut digest = Sha256::new();
            for spec in workload.reads {
                digest.update(read_digest(spec, &[0xff03_0201; 8]));
            }
            assert_eq!(
                report.reference_checksum_sha256,
                format!("{:x}", digest.finalize())
            );
            assert_ne!(
                report.candidate_checksum_sha256,
                report.reference_checksum_sha256
            );
        }

        let mut first = true;
        let only = compare_read_plan(&plan, Some("pan_trace_l0"), |_, actual, expected| {
            *actual = vec![0xff00_0000; 8];
            *expected = actual.clone();
            // A mismatch in the first region must not be hidden by the
            // remaining matching regions in the selected workload.
            if first {
                actual[0] ^= 0x0100_0001;
                first = false;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].name, "pan_trace_l0");
        assert_eq!(
            (only[0].max_abs, only[0].max_mean_abs, only[0].alpha_exact),
            (1, 1.0 / 24.0, false)
        );
        assert_ne!(
            only[0].candidate_checksum_sha256,
            only[0].reference_checksum_sha256
        );
    }

    #[test]
    fn comparison_rejects_failed_empty_and_unequal_reads() {
        let plan = WorkloadPlan::with_level0_bounds(
            vec![LevelInfo {
                width: 1,
                height: 1,
                downsample: 1.0,
            }],
            Level0Bounds {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
        )
        .unwrap();
        assert_eq!(
            compare_read_plan(&plan, None, |_, _, _| Err("decode failed".into())).unwrap_err(),
            "decode failed"
        );
        for lengths in [(0, 0), (1, 0), (1, 2)] {
            assert!(compare_read_plan(&plan, None, |_, actual, expected| {
                actual.resize(lengths.0, 0);
                expected.resize(lengths.1, 0);
                Ok(())
            })
            .unwrap_err()
            .contains("empty or unequal"));
        }
    }

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
