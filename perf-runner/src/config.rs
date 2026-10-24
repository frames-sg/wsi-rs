use std::path::PathBuf;

/// The worker's decoded-tile cache budget when `--cache-bytes` is absent.
pub const DEFAULT_CACHE_BYTES: usize = 256 * 1024 * 1024;
/// The OpenSlide release the benchmark compares against.
pub const PINNED_OPENSLIDE_VERSION: &str = "4.0.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    WsiRs,
    OpenSlide,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Self::WsiRs => "wsi_rs",
            Self::OpenSlide => "openslide",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerConfig {
    pub engine: Engine,
    pub library_path: PathBuf,
    pub slide_path: PathBuf,
    pub repeat_index: u32,
    pub cache_bytes: usize,
    pub workers: usize,
    pub only: Option<String>,
    pub required_version_prefix: Option<String>,
    pub comparison_library: Option<PathBuf>,
}

impl WorkerConfig {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut engine = None;
        let mut library_path = None;
        let mut slide_path = None;
        let mut repeat_index = None;
        let mut cache_bytes = None;
        let mut workers = None;
        let mut only = None;
        let mut required_version_prefix = None;
        let mut comparison_library = None;
        let mut args = args.into_iter();

        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--engine" => assign_once(&mut engine, "--engine", parse_engine(&value)?)?,
                "--library" => assign_once(&mut library_path, "--library", PathBuf::from(value))?,
                "--compare-library" => assign_once(
                    &mut comparison_library,
                    "--compare-library",
                    PathBuf::from(value),
                )?,
                "--slide" => assign_once(&mut slide_path, "--slide", PathBuf::from(value))?,
                "--repeat-index" => {
                    let repeat = value
                        .parse::<u32>()
                        .map_err(|err| format!("invalid --repeat-index {value:?}: {err}"))?;
                    assign_once(&mut repeat_index, "--repeat-index", repeat)?;
                }
                "--cache-bytes" => {
                    let bytes = value
                        .parse::<usize>()
                        .map_err(|err| format!("invalid --cache-bytes {value:?}: {err}"))?;
                    if bytes == 0 {
                        return Err("--cache-bytes must be positive".into());
                    }
                    assign_once(&mut cache_bytes, "--cache-bytes", bytes)?;
                }
                "--workers" => {
                    let count = value
                        .parse::<usize>()
                        .map_err(|err| format!("invalid --workers {value:?}: {err}"))?;
                    if count == 0 {
                        return Err("--workers must be positive".into());
                    }
                    assign_once(&mut workers, "--workers", count)?;
                }
                "--only" => {
                    if value.is_empty() {
                        return Err("--only must not be empty".into());
                    }
                    assign_once(&mut only, "--only", value)?;
                }
                "--require-version-prefix" => {
                    if value.is_empty() {
                        return Err("--require-version-prefix must not be empty".into());
                    }
                    assign_once(
                        &mut required_version_prefix,
                        "--require-version-prefix",
                        value,
                    )?;
                }
                _ => return Err(format!("unknown worker argument {flag:?}")),
            }
        }

        Ok(Self {
            engine: engine.ok_or("missing required --engine")?,
            library_path: library_path.ok_or("missing required --library")?,
            slide_path: slide_path.ok_or("missing required --slide")?,
            repeat_index: repeat_index.unwrap_or(0),
            cache_bytes: cache_bytes.unwrap_or(DEFAULT_CACHE_BYTES),
            workers: workers.unwrap_or(1),
            only,
            required_version_prefix,
            comparison_library,
        })
    }

    /// The command-line arguments that [`WorkerConfig::parse`] reads back as
    /// this configuration.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            "--engine".to_string(),
            self.engine.name().to_string(),
            "--library".to_string(),
            self.library_path.display().to_string(),
            "--slide".to_string(),
            self.slide_path.display().to_string(),
            "--repeat-index".to_string(),
            self.repeat_index.to_string(),
            "--cache-bytes".to_string(),
            self.cache_bytes.to_string(),
            "--workers".to_string(),
            self.workers.to_string(),
        ];
        if let Some(workload) = &self.only {
            args.extend(["--only".to_string(), workload.clone()]);
        }
        if let Some(version) = &self.required_version_prefix {
            args.extend(["--require-version-prefix".to_string(), version.clone()]);
        }
        if let Some(library) = &self.comparison_library {
            args.extend([
                "--compare-library".to_string(),
                library.display().to_string(),
            ]);
        }
        args
    }
}

fn parse_engine(value: &str) -> Result<Engine, String> {
    match value {
        "wsi_rs" => Ok(Engine::WsiRs),
        "openslide" => Ok(Engine::OpenSlide),
        _ => Err(format!(
            "invalid --engine {value:?}; expected wsi_rs or openslide"
        )),
    }
}

fn assign_once<T>(slot: &mut Option<T>, flag: &str, value: T) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate {flag}"));
    }
    *slot = Some(value);
    Ok(())
}

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;
