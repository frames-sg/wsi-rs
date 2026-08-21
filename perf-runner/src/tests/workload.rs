use super::*;

fn workload_plan(levels: Vec<LevelInfo>) -> Result<WorkloadPlan, String> {
    let level0 = levels
        .first()
        .copied()
        .ok_or_else(|| "slide has no levels".to_string())?;
    WorkloadPlan::with_level0_bounds(
        levels,
        Level0Bounds {
            x: 0,
            y: 0,
            width: level0.width,
            height: level0.height,
        },
    )
}

#[test]
fn declared_capture_workloads_match_the_generated_plan() {
    let plan = workload_plan(vec![LevelInfo {
        width: 4_096,
        height: 4_096,
        downsample: 1.0,
    }])
    .expect("plan");
    let generated = std::iter::once(CAPTURE_WORKLOAD_NAMES[0])
        .chain(
            plan.viewer_workloads()
                .into_iter()
                .map(|workload| workload.name),
        )
        .collect::<Vec<_>>();

    assert_eq!(CAPTURE_WORKLOAD_NAMES, generated.as_slice());
}

#[test]
fn declared_capture_workloads_include_non_headline_cpu_tracks() {
    assert_eq!(
        &CAPTURE_WORKLOAD_NAMES[1..8],
        &[
            "single_tile_l0",
            "pan_trace_l0",
            "pan_trace_l2",
            "viewport_region_l2",
            "zoom_trace",
            "warm_revisit_l0",
            "cache_pressure_l0",
        ]
    );
    assert!(CAPTURE_WORKLOAD_NAMES.contains(&"large_region_l0"));
    assert!(CAPTURE_WORKLOAD_NAMES.contains(&"batch_export_l0"));
}

#[test]
fn non_headline_cpu_tracks_are_deterministic_bounded_and_unique_where_required() {
    let plan = workload_plan(vec![LevelInfo {
        width: 4_300,
        height: 2_300,
        downsample: 1.0,
    }])
    .expect("valid plan");

    let first = plan.viewer_workloads();
    let second = plan.viewer_workloads();
    let large_region = first
        .iter()
        .find(|workload| workload.name == "large_region_l0")
        .expect("large-region workload");
    let repeated_large_region = second
        .iter()
        .find(|workload| workload.name == "large_region_l0")
        .expect("repeated large-region workload");
    let batch_export = first
        .iter()
        .find(|workload| workload.name == "batch_export_l0")
        .expect("batch-export workload");
    let repeated_batch_export = second
        .iter()
        .find(|workload| workload.name == "batch_export_l0")
        .expect("repeated batch-export workload");

    assert_eq!(large_region, repeated_large_region);
    assert_eq!(batch_export, repeated_batch_export);
    assert!(!large_region.reads.is_empty());
    assert!(!batch_export.reads.is_empty());
    assert!(large_region
        .reads
        .iter()
        .all(|read| read.level == 0 && read.width == 2_048 && read.height == 2_048));
    assert!(batch_export
        .reads
        .iter()
        .all(|read| read.level == 0 && read.width <= 256 && read.height <= 256));

    for read in large_region.reads.iter().chain(&batch_export.reads) {
        assert!(read.width > 0 && read.height > 0);
        assert!(read.x >= 0 && read.y >= 0);
        assert!(read.x as u64 + u64::from(read.width) <= plan.levels[0].width);
        assert!(read.y as u64 + u64::from(read.height) <= plan.levels[0].height);
    }

    assert_eq!(batch_export.reads.len(), 153);
    assert_eq!(
        batch_export.reads.last(),
        Some(&ReadSpec {
            x: 4_096,
            y: 2_048,
            level: 0,
            width: 204,
            height: 252,
        })
    );
    assert_eq!(
        batch_export
            .reads
            .iter()
            .map(|read| (read.x, read.y, read.level, read.width, read.height))
            .collect::<std::collections::HashSet<_>>()
            .len(),
        batch_export.reads.len()
    );
}

#[test]
fn percentile_uses_nearest_rank_and_rejects_invalid_inputs() {
    let samples = [10, 20, 30, 40];

    assert_eq!(percentile(&samples, 50), Some(20));
    assert_eq!(percentile(&samples, 95), Some(40));
    assert_eq!(percentile(&samples, 99), Some(40));
    assert_eq!(percentile(&[], 50), None);
    assert_eq!(percentile(&samples, 0), None);
    assert_eq!(percentile(&samples, 101), None);
}

#[test]
fn sample_summary_sorts_a_copy_and_reports_integer_microseconds() {
    let samples = [40, 10, 30, 20];

    assert_eq!(
        summarize_samples(&samples),
        Some(SampleSummary {
            p50_us: 20,
            p95_us: 40,
            p99_us: 40,
            mean_us: 25,
        })
    );
    assert_eq!(samples, [40, 10, 30, 20]);
    assert_eq!(summarize_samples(&[]), None);
}

#[test]
fn workload_plan_requires_valid_ordered_levels() {
    assert_eq!(workload_plan(Vec::new()), Err("slide has no levels".into()));
    assert!(workload_plan(vec![LevelInfo {
        width: 0,
        height: 10,
        downsample: 1.0,
    }])
    .is_err());
    assert!(workload_plan(vec![LevelInfo {
        width: 10,
        height: 10,
        downsample: f64::NAN,
    }])
    .is_err());
    assert!(workload_plan(vec![
        LevelInfo {
            width: 10,
            height: 10,
            downsample: 2.0,
        },
        LevelInfo {
            width: 5,
            height: 5,
            downsample: 1.0,
        },
    ])
    .is_err());
    assert!(workload_plan(vec![
        LevelInfo {
            width: 10,
            height: 10,
            downsample: 1.0,
        },
        LevelInfo {
            width: 5,
            height: 5,
            downsample: 2.0,
        },
    ])
    .is_ok());
}

#[test]
fn tissue_bounds_constrain_every_workload_in_level_zero_coordinates() {
    let bounds = Level0Bounds {
        x: 9_919,
        y: 89_420,
        width: 68_315,
        height: 94_972,
    };
    let plan = WorkloadPlan::with_level0_bounds(
        vec![
            LevelInfo {
                width: 100_000,
                height: 220_000,
                downsample: 1.0,
            },
            LevelInfo {
                width: 25_000,
                height: 55_000,
                downsample: 4.0,
            },
        ],
        bounds,
    )
    .expect("valid bounded plan");

    assert_eq!(plan.level0_bounds, bounds);
    let workloads = plan.viewer_workloads();
    let single = &workloads[0].reads[0];
    assert_eq!(
        (single.x, single.y),
        (
            bounds.x + i64::try_from((bounds.width - 256) / 4).unwrap(),
            bounds.y + i64::try_from((bounds.height - 256) / 4).unwrap(),
        )
    );
    let pan = &workloads[1].reads;
    assert_eq!((pan[0].x, pan[0].y), (bounds.x, bounds.y));
    assert!(pan.last().unwrap().x > pan[0].x);
    assert!(pan.last().unwrap().y > pan[0].y);

    for workload in workloads {
        for read in workload.reads {
            let downsample = plan.levels[read.level as usize].downsample;
            let right = read.x as f64 + f64::from(read.width) * downsample;
            let bottom = read.y as f64 + f64::from(read.height) * downsample;
            assert!(
                read.x >= bounds.x,
                "{} starts left of tissue",
                workload.name
            );
            assert!(read.y >= bounds.y, "{} starts above tissue", workload.name);
            assert!(
                right <= bounds.x as f64 + bounds.width as f64 + downsample,
                "{} extends right of tissue",
                workload.name
            );
            assert!(
                bottom <= bounds.y as f64 + bounds.height as f64 + downsample,
                "{} extends below tissue",
                workload.name
            );
        }
    }
}

#[test]
fn tissue_bounds_reject_empty_or_overflowing_extents() {
    let levels = vec![LevelInfo {
        width: 4_096,
        height: 4_096,
        downsample: 1.0,
    }];
    assert!(WorkloadPlan::with_level0_bounds(
        levels.clone(),
        Level0Bounds {
            x: 0,
            y: 0,
            width: 0,
            height: 1,
        },
    )
    .is_err());
    assert!(WorkloadPlan::with_level0_bounds(
        levels,
        Level0Bounds {
            x: i64::MAX,
            y: 0,
            width: 2,
            height: 1,
        },
    )
    .is_err());
}

#[test]
fn viewer_workloads_are_deterministic_and_within_level_bounds() {
    let plan = workload_plan(vec![
        LevelInfo {
            width: 4_096,
            height: 2_048,
            downsample: 1.0,
        },
        LevelInfo {
            width: 2_048,
            height: 1_024,
            downsample: 2.0,
        },
        LevelInfo {
            width: 1_024,
            height: 512,
            downsample: 4.0,
        },
    ])
    .expect("valid plan");

    let first = plan.viewer_workloads();
    let second = plan.viewer_workloads();

    assert_eq!(first, second);
    assert_eq!(first[0].name, "single_tile_l0");
    assert_eq!(first[0].reads.len(), 128);
    assert_eq!(first[1].reads.first().expect("pan start").x, 0);
    assert_eq!(first[1].reads.last().expect("pan end").x, 3_840);
    assert_eq!(first[2].reads[0].level, 2);
    assert_eq!(first[3].reads[0].width, 1_024);
    assert_eq!(first[3].reads[0].height, 512);
    for workload in &first {
        for read in &workload.reads {
            let level = plan.levels[read.level as usize];
            assert!(read.x >= 0);
            assert!(read.y >= 0);
            let max_x = level.width as f64 * level.downsample;
            let max_y = level.height as f64 * level.downsample;
            assert!(read.x as f64 + f64::from(read.width) * level.downsample <= max_x + 1.0);
            assert!(read.y as f64 + f64::from(read.height) * level.downsample <= max_y + 1.0);
        }
    }
}

#[test]
fn every_headline_viewer_workload_has_enough_samples_for_p99() {
    let plan = workload_plan(vec![LevelInfo {
        width: 8_192,
        height: 8_192,
        downsample: 1.0,
    }])
    .expect("valid plan");

    for workload in plan.viewer_workloads().into_iter().take(7) {
        assert!(
            workload.reads.len() >= 100,
            "{} has only {} samples",
            workload.name,
            workload.reads.len()
        );
    }
}

#[test]
fn cache_pressure_plan_exceeds_default_decoded_budget_and_supports_p99() {
    let plan = workload_plan(vec![LevelInfo {
        width: 100_000,
        height: 100_000,
        downsample: 1.0,
    }])
    .expect("valid plan");

    let workload = plan
        .viewer_workloads()
        .into_iter()
        .find(|workload| workload.name == "cache_pressure_l0")
        .expect("cache-pressure workload");
    let decoded_bytes = workload.reads.len() * 256 * 256 * 4;

    assert!(workload.reads.len() >= 100);
    assert!(decoded_bytes > 256 * 1024 * 1024);
    assert_eq!(
        workload
            .reads
            .iter()
            .map(|read| (read.x, read.y, read.level, read.width, read.height))
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        workload.reads.len()
    );
}

#[test]
fn cache_pressure_plan_cycles_deterministically_on_small_slides() {
    let plan = workload_plan(vec![LevelInfo {
        width: 512,
        height: 512,
        downsample: 1.0,
    }])
    .expect("valid plan");

    let first = plan
        .viewer_workloads()
        .into_iter()
        .find(|workload| workload.name == "cache_pressure_l0")
        .expect("cache-pressure workload");
    let second = plan
        .viewer_workloads()
        .into_iter()
        .find(|workload| workload.name == "cache_pressure_l0")
        .expect("cache-pressure workload");

    assert_eq!(first, second);
    assert_eq!(first.reads[0], first.reads[4]);
    assert_eq!(first.reads.len(), 1_025);
}
