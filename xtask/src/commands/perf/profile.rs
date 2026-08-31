use std::path::{Path, PathBuf};

use super::capture::worker_count;
use super::worker::{
    cache_bytes, shim_library_name, target_directory, worker_args, workspace_root, BenchInvocation,
    BenchLibrary, DEFAULT_CACHE_BYTES, RAYON_NUM_THREADS_ENV,
};

const PROFILE_DIR_ENV: &str = "WSI_RS_PERF_PROFILE_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileRecipes {
    cpu_samply: Vec<String>,
    cpu_time_profiler: Vec<String>,
}

pub(in crate::commands) fn profile(args: Vec<String>) -> Result<(), String> {
    let Some(slide_arg) = args.first() else {
        return Err("usage: cargo xtask perf-profile <slide-path> [workload-name]".into());
    };
    if args.len() > 2 {
        return Err("usage: cargo xtask perf-profile <slide-path> [workload-name]".into());
    }
    let slide = PathBuf::from(slide_arg);
    if !slide.is_file() {
        return Err(format!("profile slide is not a file: {}", slide.display()));
    }
    let workload = args.get(1).map(String::as_str);
    let label = profile_label(&slide, workload);
    let recipes = profile_recipes(&slide, workload, &label);

    println!(
        "Build first:\n  cargo build --release -p wsi-rs-perf -p wsi-rs-openslide-shim\n\n\
         CPU samply:\n  {}\n\n\
         CPU xctrace Time Profiler:\n  {}",
        shell_join(&recipes.cpu_samply),
        shell_join(&recipes.cpu_time_profiler)
    );
    Ok(())
}

fn profile_recipes(slide: &Path, workload: Option<&str>, label: &str) -> ProfileRecipes {
    let profile_dir = std::env::var_os(PROFILE_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("bench/results/profiles"));
    let target_dir = target_directory().unwrap_or_else(|_| workspace_root().join("target"));
    let invocation = BenchInvocation {
        worker: target_dir
            .join("release")
            .join(format!("wsi-rs-perf{}", std::env::consts::EXE_SUFFIX)),
        library: target_dir.join("release").join(shim_library_name()),
    };
    let workers = worker_count().unwrap_or(1);
    let mut bench_invocation = worker_args(
        BenchLibrary::WsiRs,
        &invocation,
        slide,
        0,
        cache_bytes().unwrap_or(DEFAULT_CACHE_BYTES),
        workers,
        workload,
    );
    bench_invocation.splice(
        0..0,
        [
            "env".to_string(),
            format!("{RAYON_NUM_THREADS_ENV}={workers}"),
            invocation.worker.display().to_string(),
        ],
    );

    let mut xctrace_bench_invocation = bench_invocation.clone();
    if cfg!(target_os = "macos") {
        // xctrace treats the first launch argument as a literal path rather
        // than resolving it through PATH.
        xctrace_bench_invocation[0] = "/usr/bin/env".into();
    }
    let mut cpu_samply = vec![
        "samply".to_string(),
        "record".to_string(),
        "--save-only".to_string(),
        "--output".to_string(),
        profile_dir
            .join(format!("{label}-samply.json.gz"))
            .display()
            .to_string(),
        "--profile-name".to_string(),
        label.to_string(),
    ];
    cpu_samply.extend(bench_invocation);

    let mut cpu_time_profiler = vec![
        "xcrun".to_string(),
        "xctrace".to_string(),
        "record".to_string(),
        "--template".to_string(),
        "Time Profiler".to_string(),
        "--output".to_string(),
        profile_dir
            .join(format!("{label}-time-profiler.trace"))
            .display()
            .to_string(),
        "--launch".to_string(),
        "--".to_string(),
    ];
    cpu_time_profiler.extend(xctrace_bench_invocation);

    ProfileRecipes {
        cpu_samply,
        cpu_time_profiler,
    }
}

fn profile_label(slide: &Path, workload: Option<&str>) -> String {
    let stem = slide
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("slide");
    let workload = workload.unwrap_or("full-suite");
    sanitize_label(&format!("{stem}-{workload}"))
}

fn sanitize_label(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(arg: &str) -> String {
    if arg
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '-' | '_' | '=' | ':'))
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
#[path = "tests/profile.rs"]
mod tests;
