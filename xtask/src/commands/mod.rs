mod checks;
mod coverage;
mod perf;
mod process;

use std::env;

pub(crate) fn run() -> Result<(), String> {
    let task = env::args().nth(1).unwrap_or_else(|| "help".to_string());
    match task.as_str() {
        "fmt" => checks::fmt(),
        "clippy" => checks::clippy(),
        "test" => checks::test(),
        "nextest" => checks::nextest(),
        "bench-check" => checks::bench_check(),
        "bench" => checks::bench(),
        "feature-check" => checks::feature_check(),
        "parity-corpus-test" => checks::parity_corpus_test(),
        "doc" | "docs" => checks::doc(),
        "doc-test" => checks::doc_test(),
        "typos" => checks::typos(),
        "deny" => checks::deny(),
        "unused-deps" => checks::unused_deps(),
        "deps" => checks::deps(),
        "api-check" => checks::api_check(),
        "fuzz-check" => checks::fuzz_check(),
        "release-test" => checks::release_test(),
        "coverage" => checks::coverage(),
        "coverage-changed" => coverage::changed(env::args().skip(2).collect()),
        "perf-capture" => perf::capture(env::args().skip(2).collect()),
        "perf-capture-openslide" => perf::capture_openslide(env::args().skip(2).collect()),
        "perf-compare" => perf::compare(env::args().skip(2).collect()),
        "perf-profile" => perf::profile(env::args().skip(2).collect()),
        "package" => checks::package(),
        "validate" => checks::validate(),
        "rc-preflight" => checks::rc_preflight(),
        "ci" => checks::ci(),
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => Err(format!("unknown task `{other}`")),
    }
}

fn print_help() {
    println!("{}", help_text());
}

fn help_text() -> &'static str {
    "usage: cargo xtask <task>\n\n\
       tasks:\n\
       ci           validate plus package\n\
       validate     fmt, clippy, bench-check, nextest, doctests, and docs\n\
       rc-preflight run local release-candidate preflight gates\n\
       fmt          check rustfmt\n\
       clippy       run clippy with warnings denied\n\
       test         run library and integration tests\n\
       nextest      run library and integration tests with cargo-nextest\n\
       bench-check  compile Rust benchmark targets without running timings\n\
       bench        run synthetic Rust Criterion benchmarks locally\n\
       feature-check check supported feature combinations with cargo-hack\n\
       parity-corpus-test run strict corpus-backed ignored integration tests\n\
       doc          build docs with warnings denied\n\
       doc-test     compile rustdoc examples with doctest\n\
       typos        run typos\n\
       deny         run cargo-deny advisories, bans, licenses, and sources checks\n\
       unused-deps  run cargo-machete for unused dependency checks\n\
       deps         run deny and unused-deps\n\
       api-check    run public API and semver stability checks\n\
       fuzz-check   type-check cargo-fuzz targets\n\
       release-test run release-mode library and integration tests\n\
       coverage     generate lcov.info with cargo-llvm-cov\n\
       coverage-changed [--base REV] [--lcov lcov.info] enforce changed-path coverage\n\
       perf-capture <label> [slides...] capture local statumen benchmark JSON\n\
       perf-capture-openslide <label> [slides...] capture local OpenSlide benchmark JSON\n\
       perf-compare <before.json> <after.json> compare captures with 5% noise guard\n\
       perf-profile <slide> [workload] print samply/xctrace profiling recipes\n\
       package      package the crate from a clean worktree with verification"
}

#[cfg(test)]
mod tests {
    use super::help_text;

    #[test]
    fn help_lists_benchmark_tasks() {
        let help = help_text();

        assert!(help.contains("bench-check"));
        assert!(help.contains("bench        "));
        assert!(help.contains("perf-capture"));
        assert!(help.contains("perf-capture-openslide"));
        assert!(help.contains("perf-compare"));
        assert!(help.contains("perf-profile"));
        assert!(help.contains("coverage-changed"));
    }

    #[test]
    fn help_lists_api_stability_task() {
        let help = help_text();

        assert!(help.contains("api-check    run public API and semver stability checks"));
    }

    #[test]
    fn help_lists_doc_test_task() {
        let help = help_text();

        assert!(help.contains("doc-test     compile rustdoc examples with doctest"));
    }

    #[test]
    fn help_lists_fuzzing_task() {
        let help = help_text();

        assert!(help.contains("fuzz-check   type-check cargo-fuzz targets"));
    }

    #[test]
    fn package_help_advertises_package_verification() {
        let help = help_text();

        assert!(
            help.contains("package      package the crate from a clean worktree with verification")
        );
        assert!(!help.contains("without verification"));
    }

    #[test]
    fn help_lists_rc_preflight_task() {
        let help = help_text();

        assert!(help.contains("rc-preflight run local release-candidate preflight gates"));
    }
}
