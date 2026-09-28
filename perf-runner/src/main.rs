use std::process::ExitCode;

use wsi_rs_perf::{compare_pixels, run, WorkerConfig};

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wsi-rs-perf: {error}");
            ExitCode::from(2)
        }
    }
}

fn execute() -> Result<(), String> {
    let config = WorkerConfig::parse(std::env::args().skip(1))?;
    let result = if config.comparison_library.is_some() {
        serde_json::to_value(compare_pixels(&config)?)
    } else {
        serde_json::to_value(run(&config)?)
    }
    .map_err(|err| format!("failed to encode worker JSON: {err}"))?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &result)
        .map_err(|err| format!("failed to write worker JSON: {err}"))?;
    println!();
    Ok(())
}
