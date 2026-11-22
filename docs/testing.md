# Testing

How to validate a change, run the explicit conformance and performance lanes, and pass the release
checks. For how the code is organized, see [Architecture](architecture.md).

## Validation

Run these from the repository root:

```sh
cargo xtask validate      # format, clippy, nextest, doc tests and docs (alias: cargo run-checks)
cargo xtask bench-check   # build the release performance worker and OpenSlide shim
cargo xtask fuzz-check    # type-check every fuzz target
cargo xtask feature-check # check the feature powerset, except metal and parity-metal
cargo xtask api-check     # compare the public API with the snapshots in api/
cargo xtask deps          # cargo-deny, cargo-machete and cargo-vet
```

`cargo xtask help` lists every task.

`validate` runs the library and integration tests once, then the OpenSlide oracle tests with the
`parity-openslide` feature. On macOS it also runs the library suite and the Metal surface check with
`parity-metal`. Real-slide comparisons and timing runs are ignored tests that only run through the
lanes below. Each lane lists the ignored tests its selection matches and fails if there are none,
and an xtask test fails when an ignored test belongs to no lane.

## Test Layout

- **Unit tests** live in each module's `tests` module or `tests/` folder.
- **Integration tests** in `tests/` read synthetic fixtures and real slides.
- **Repository policy** tests in `tests/repo_policy/` check the unsafe-code policy, doc language and
  tracked files.
- **Test hooks.** `core::execution_telemetry` counts events such as tiles decoded on each path, and
  `core::test_hooks` pauses work at fixed points to force races. Both compile to nothing in normal
  builds. The `route-telemetry` feature exports the counts for benchmarks.

## OpenSlide Comparison

`scripts/parity-corpus-fetch.sh` downloads a public corpus of real slides into
`~/.cache/wsi-rs/parity-corpus` (or `WSI_RS_PARITY_CORPUS_CACHE`). `cargo xtask parity-corpus-test`
reads every slide with wsi-rs and compares pixels with OpenSlide where it supports the format. It
also covers DICOM folder opening, Olympus VSI reading, and SVS, NDPI and DICOM color checks, which
require every selected sample.

`WSI_RS_PARITY_ALIASES` limits the run to the manifest aliases it lists, separated by commas,
semicolons or whitespace; unset or empty selects the whole corpus.

When Olympus VSI or Zeiss ZVI is selected, the run also compares native pixels with probes decoded
by the independent SlideIO 2.9.0 reader. Generate them after fetching the two samples:

```sh
scripts/parity-corpus-fetch.sh olympus-vsi-001 zeiss-zvi-001
uv run --python 3.13 --with slideio==2.9.0 scripts/prepare-vendor-reference.py \
  --output target/vendor-reference
WSI_RS_VENDOR_REFERENCE_ROOT=target/vendor-reference cargo xtask conformance-test vendor
```

The script finds both samples in the corpus cache; `--vsi` and `--zvi` override the paths. ZVI
references must cover all three native 16-bit channels. Corpus CI and source release preflight
create a pinned SlideIO environment and run this comparison. Hamamatsu VMU has synthetic coverage
only.

## Conformance Lanes

`cargo xtask conformance-test <suite>` runs ignored tests that need fixtures or hardware. Missing
inputs or hardware fail the run.

| Suite           | Required input and checks                                                                                                                                                                                                                                                                                            |
| --------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `czi`           | `WSI_RS_ZEISS_CZI_PATH` (Zeiss-5-Uncompressed.czi) and `WSI_RS_CZI_JXR_PATH` (Zeiss-5-JXR.czi): canvas and cache regressions and JPEG XR readback.                                                                                                                                                                   |
| `vendor`        | `WSI_RS_VENDOR_REFERENCE_ROOT` from the generator above: VSI RGB and ZVI 16-bit pixels.                                                                                                                                                                                                                              |
| `zvi-variants`  | `WSI_RS_ZVI_ROOT` holding the six files named in `tests/zeiss_zvi.rs`: channels, Z planes, mosaic geometry, thumbnails and tile locations.                                                                                                                                                                           |
| `vmu`           | libopenslide. The lane builds the shim and sets `WSI_RS_VMU_SHIM_LIBRARY`: generated RGB12 geometry and C ABI pixel parity.                                                                                                                                                                                          |
| `openslide`     | libopenslide, `WSI_RS_OPENSLIDE_COMPARE_PATHS` and `WSI_RS_APERIO_JPEG_RGB_PATH`: region and reduced-level RGBA comparisons of the native API. Leave out Leica slides, whose native levels cover each scene rather than the whole collection, and Ventana slides, whose native reads are opaque at fractional edges. |
| `metal-color`   | macOS Metal: exhaustive YCbCr8 parity for both addressing kernels.                                                                                                                                                                                                                                                   |
| `metal-edges`   | macOS Metal and `WSI_RS_METAL_EDGE_PATHS`; each source must have a regular eligible edge tile.                                                                                                                                                                                                                       |
| `local-htj2k`   | macOS Metal and `WSI_RS_LOCAL_HTJ2K_DICOM`: HTJ2K device pixel parity.                                                                                                                                                                                                                                               |
| `legacy-slides` | `WSI_RS_NDPI_WRAPPED_FIXTURE` (Hamamatsu-1.ndpi), `WSI_RS_LEICA_MULTI_SCENE_FIXTURE` (Leica-3.scn), `WSI_RS_LEICA_FLUORESCENCE_FIXTURE` (Leica-Fluorescence-1.scn) and `WSI_RS_DICOM_SPLIT_FIXTURE` (3DHISTECH-2 split-level folder): format regressions.                                                            |
| `ome-tiff`      | `WSI_RS_OME_TIFF_FIXTURE`, a single-file OME-TIFF: SubIFD pyramid levels and full- and lowest-resolution tiles of every channel.                                                                                                                                                                                     |

## Performance Experiments

`cargo xtask perf-experiment <name>` builds the test in release mode with route telemetry and runs
only that experiment. Use a separate output file for each run, and compare captures only under the
same corpus, cache and thread conditions.

| Experiment                                                     | Required environment                                                                    |
| -------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `metal_window_performance`, `metal_cooperative_performance`    | macOS Metal, `WSI_RS_PREPARE_PATH` (a JPEG 2000 slide), `WSI_RS_PREPARE_OUTPUT`         |
| `pooled_cpu_scheduling_performance`                            | `WSI_RS_PREPARE_PATH`, `WSI_RS_PREPARE_OUTPUT`                                          |
| `ycbcr_selected_u32_stays_within_five_percent_of_reference`    | macOS Metal; five-run address-width check                                               |
| `composition_performance`                                      | `WSI_RS_COMPOSITION_OUTPUT`                                                             |
| `native_decode_matrix`                                         | `WSI_RS_DECODE_PATH`, `WSI_RS_DECODE_OUTPUT`; optional modes are documented in the test |
| `native_decode_revisits`, `native_decode_fresh_readers`        | `WSI_RS_DECODE_STEADY_INPUT`, `WSI_RS_DECODE_OUTPUT`                                    |
| `panning_matrix`                                               | `WSI_RS_PAN_PATH`, `WSI_RS_PAN_OUTPUT`                                                  |
| `warm_region_revisits`                                         | `WSI_RS_PAN_WARM_INPUT`, `WSI_RS_PAN_WARM_OUTPUT`                                       |
| `czi_reader_workloads`                                         | `WSI_RS_CZI_JXR_PATH`, `WSI_RS_CZI_PERF_OUTPUT`                                         |
| `local_htj2k_dicom_level_preparation_meets_interactive_budget` | macOS and `WSI_RS_LOCAL_HTJ2K_DICOM`; 75 ms preparation budget                          |

`metal_cooperative_performance` alternates ordinary and cooperative Metal over five pairs for 4, 8
and 16 tiles, 4, 8 and 16 MiB windows, and several worker budgets. It checks pixels against the CPU
outside the timed interval. A report with `cooperative_was_exercised: false` measured the GPU
fallback, not cooperative decoding.

## Fuzzing

`fuzz/fuzz_targets` has one target per parser. `cargo xtask fuzz-check` checks that each compiles.
CI runs each target for 15 seconds; `cargo xtask release-fuzz` runs each for five minutes under
AddressSanitizer.

## Coverage

CI requires 80% line coverage across the workspace, 70% in each major component, and 80% on changed
lines (`cargo xtask coverage-changed`).

## Release Checks

`cargo xtask rc-preflight` runs, in order: the configured artifact smoke test, `api-check`, `deps`,
`fuzz-check`, five-minute fuzz runs, `feature-check`, `validate`, `bench-check`, release-mode tests,
the OpenSlide corpus comparison, coverage with the corpus, the performance gate and a package dry
run.

Releases with the `cuda` feature also need the `CUDA validation` workflow on the CUDA runner. The
`RC Preflight` workflow takes `performance_release_tag`, a GitHub release holding `current.json.gz`,
`previous.json.gz` and `openslide.json.gz` from a reviewed paired capture; the current capture must
name the same commit and a clean checkout. To rebuild only the C artifacts, dispatch it with
`artifacts_only` and `source_preflight_run_id` set to a run whose source checks passed on the same
commit.

## Performance Gate

The gate compares three captures made on the same machine with the same corpus, workloads, cache
size and thread limits: OpenSlide 4.0.1, the previous wsi-rs release and the current code. Build
both wsi-rs versions with the same Rust toolchain and release profile. When you supply a prebuilt
library through `WSI_RS_BENCH_WSI_RS_LIBRARY`, check its build settings yourself; the capture's
toolchain metadata describes the running environment, not that library.

1. Set `WSI_RS_OPENSLIDE_LIBRARY` to libopenslide 4.0.1 and `WSI_RS_PERF_PINNED_HOST_ID` to a fixed
   name for the machine. To build the current shim with a GPU feature, set `WSI_RS_PERF_GPU_FEATURE`
   to `metal` or `cuda`. `WSI_RS_PERF_MANIFEST` and `WSI_RS_PERF_RESULTS_DIR` override the workload
   manifest and the output folder.
2. Set `WSI_RS_BENCH_PREVIOUS_LIBRARY` to the previous release's shim and run
   `cargo xtask perf-capture-pair <label>`. It interleaves the three libraries for each sample,
   worker count and repeat, reversing their order on alternate repeats, and writes
   `<label>-wsi_rs.json`, `<label>-openslide.json` and `<label>-previous.json`. Each capture records
   the order the libraries ran in; the previous-release capture records that shim's path, SHA-256
   and version. Without `WSI_RS_BENCH_PREVIOUS_LIBRARY`, it captures only the current code and
   OpenSlide.
3. Point `WSI_RS_RC_OPENSLIDE_CAPTURE`, `WSI_RS_RC_PREVIOUS_CAPTURE` and `WSI_RS_RC_CURRENT_CAPTURE`
   at the three files. For the workflow, publish them as `openslide.json.gz`, `previous.json.gz` and
   `current.json.gz` on the `performance_release_tag` release.

The gate fails when:

- the viewer p50 geometric-mean ratio against OpenSlide exceeds 1.00, or any viewer p50, p95 or p99
  cell exceeds 1.05;
- peak RSS exceeds 1.20× OpenSlide's in any cell;
- against the previous release, in at least 3 of 5 repeats, a latency (p50, p95, p99) grows more
  than 5%, throughput drops more than 5% or peak RSS grows more than 10%;
- pixels differ from OpenSlide beyond the corpus color tolerances;
- any wsi-rs cell decoded on a GPU: every run must report `device_decode` counters, all zero;
- the captures were made by different worker versions, so capture all three in one run.
