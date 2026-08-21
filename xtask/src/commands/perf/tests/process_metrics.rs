use super::*;

#[test]
fn resource_usage_parsers_capture_cpu_time_on_both_benchmark_hosts() {
    assert_eq!(
        parse_macos_cpu_seconds("  1.25 real  0.75 user  0.10 sys\n"),
        Some((0.75, 0.10))
    );
    assert_eq!(
        parse_linux_cpu_seconds("User time (seconds): 0.75\nSystem time (seconds): 0.10\n"),
        Some((0.75, 0.10))
    );
    assert_eq!(parse_macos_cpu_seconds("not time output"), None);
    assert_eq!(parse_linux_cpu_seconds("User time (seconds): 0.75\n"), None);
}

#[test]
fn resource_usage_annotation_records_supported_host_metrics() {
    let stderr = if cfg!(target_os = "macos") {
        "1.25 real 0.75 user 0.10 sys\n12345 maximum resident set size\n"
    } else {
        "User time (seconds): 0.75\nSystem time (seconds): 0.10\nMaximum resident set size (kbytes): 12\n"
    };
    let mut run = serde_json::json!({"kind": "run"});

    annotate_run_resource_usage(&mut run, stderr.as_bytes());

    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert_eq!(run["cpu_user_seconds"], 0.75);
        assert_eq!(run["cpu_system_seconds"], 0.10);
        assert_eq!(run["cpu_time_seconds"], 0.85);
        assert!(run["cpu_time_method"].is_string());
        assert!(run[PEAK_RSS_METRIC].as_u64().is_some_and(|rss| rss > 0));
        assert!(run["rss_method"].is_string());
    }

    let mut scalar = serde_json::json!(1);
    annotate_run_resource_usage(&mut scalar, stderr.as_bytes());
    assert_eq!(scalar, 1);
}

#[test]
fn peak_rss_parser_rejects_malformed_values() {
    assert_eq!(parse_peak_rss_bytes("not timing output"), None);
    let malformed = if cfg!(target_os = "macos") {
        "wat maximum resident set size"
    } else {
        "Maximum resident set size (kbytes): wat"
    };
    assert_eq!(parse_peak_rss_bytes(malformed), None);
}
