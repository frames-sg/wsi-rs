use super::schema::CaptureRun;

pub(super) const PEAK_RSS_METRIC: &str = "peak_rss_bytes";
const MACOS_TIME_METHOD: &str = "macos:/usr/bin/time -l";

pub(super) fn annotate_run_resource_usage_typed(run: &mut CaptureRun, stderr: &[u8]) {
    let stderr = String::from_utf8_lossy(stderr);
    if let Some((rss, method)) = parse_peak_rss_bytes(&stderr) {
        run.peak_rss_bytes = Some(rss);
        run.rss_method = Some(method.into());
    }
    if let Some((user, system, method)) = parse_cpu_seconds(&stderr) {
        run.cpu_user_seconds = Some(user);
        run.cpu_system_seconds = Some(system);
        run.cpu_time_seconds = Some(user + system);
        run.cpu_time_method = Some(method.into());
    }
}

#[cfg(test)]
fn annotate_run_resource_usage(run: &mut serde_json::Value, stderr: &[u8]) {
    let Ok(mut typed) = serde_json::from_value::<CaptureRun>(run.clone()) else {
        return;
    };
    annotate_run_resource_usage_typed(&mut typed, stderr);
    if let Ok(value) = serde_json::to_value(typed) {
        *run = value;
    }
}

fn parse_cpu_seconds(stderr: &str) -> Option<(f64, f64, &'static str)> {
    if cfg!(target_os = "macos") {
        return parse_macos_cpu_seconds(stderr)
            .map(|(user, system)| (user, system, MACOS_TIME_METHOD));
    }
    if cfg!(target_os = "linux") {
        return parse_linux_cpu_seconds(stderr)
            .map(|(user, system)| (user, system, "linux:/usr/bin/time -v"));
    }
    None
}

fn parse_macos_cpu_seconds(stderr: &str) -> Option<(f64, f64)> {
    stderr.lines().find_map(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let user_index = fields.iter().position(|field| *field == "user")?;
        let system_index = fields.iter().position(|field| *field == "sys")?;
        let user = fields.get(user_index.checked_sub(1)?)?.parse().ok()?;
        let system = fields.get(system_index.checked_sub(1)?)?.parse().ok()?;
        Some((user, system))
    })
}

fn parse_linux_cpu_seconds(stderr: &str) -> Option<(f64, f64)> {
    let value = |prefix: &str| {
        stderr
            .lines()
            .find_map(|line| line.trim().strip_prefix(prefix)?.trim().parse::<f64>().ok())
    };
    Some((
        value("User time (seconds):")?,
        value("System time (seconds):")?,
    ))
}

fn parse_peak_rss_bytes(stderr: &str) -> Option<(u64, &'static str)> {
    if cfg!(target_os = "macos") {
        return stderr.lines().find_map(|line| {
            line.contains("maximum resident set size").then(|| {
                line.split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
                    .map(|bytes| (bytes, MACOS_TIME_METHOD))
            })?
        });
    }
    if cfg!(target_os = "linux") {
        return stderr.lines().find_map(|line| {
            line.trim()
                .strip_prefix("Maximum resident set size (kbytes):")?
                .trim()
                .parse::<u64>()
                .ok()
                .and_then(|kib| kib.checked_mul(1024))
                .map(|bytes| (bytes, "linux:/usr/bin/time -v"))
        });
    }
    None
}

#[cfg(test)]
#[path = "tests/process_metrics.rs"]
mod tests;
