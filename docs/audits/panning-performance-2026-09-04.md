# DICOM, NDPI, and SVS panning — 2026-09-04

## Scope and decision

NDPI now borrows relative MCU offsets from the TIFF container's existing decoded
tag array. Its existing MCU cache stores a 128-byte classification entry rather
than another copy of that array. This avoids repeated array copies and, when the
classification fits, repeated whole-table scans. No codec, public API, dependency,
thread count, or cache capacity changed.

DICOM and SVS have no production changes from this task. Their measured panning
paths were largely decoder-bound; the profiles did not justify another reader
cache, a metadata refactor, or a new I/O scheduler. Timing differences in those
formats are controls for machine variation, not claimed improvements.

### NDPI

CMU-1.ndpi has 119,200 level-0 MCU offsets (953,600 bytes as u64 values). Native
source tiles are 2048 × 8 RGB pixels. The old reader copied and classified the
whole offset table when its normalized-table cache missed. The default 1 MiB MCU
cache could retain it, but the 16 KiB MCU allocation under a 1 MiB shared-tile
configuration could not. A viewport touching many strips therefore repeatedly
copied and scanned the same table. TIFF already retains the decoded tag array
under its existing metadata/index limits.

`ndpi_core.rs` now returns either a borrowed relative table or the existing owned
normalized table. `caches.rs` retains relative/normalized classifications in the
same weighted LRU. The relative marker has explicit nonzero accounting, including
bookkeeping; disabled caches cannot retain zero-weight entries. High-word
combination, file-absolute detection, per-segment bounds and ordering validation,
source identity checks, and decode/crop/composition behavior are unchanged.

The change does not increase strip retention. When the marker is evicted, the
reader reclassifies; when caching is disabled, it rescans but still avoids the
copy. Concurrent misses can independently classify immutable offsets. No decode
lock or new concurrency mechanism was added. Normalized tables keep their old
byte accounting and eviction behavior. The extra tag lookup on a relative cache
hit is a potential cost; default-cache measurements are reported without claiming
a general speedup.

### DICOM

Inspection and existing regression tests confirm lazy, cancellation-aware frame
index publication and reuse, grouped bounded fragment reads, Item-header
revalidation, decoded-frame caching, sparse black tiles, and batch result-slot
restoration. The sampled JPEG 2000 workload spends most non-wait/non-benchmark
samples in external j2k bitplane decoding and inverse transforms. Cold frame-index
construction remains visible in first-read latency. No validation was removed to
reduce it. No JPEG DICOM performance claim is made: the manifest's JPEG sample
was not available locally.

### SVS

TIFF already resolves metadata/offset arrays once and uses positional reads.
Shared source-tile caching reuses overlapping tiles; decode batches and clipped
composition already exist. JPEG profiles are dominated by external JPEG entropy
and IDCT work; JPEG 2000 profiles by external bitplane decoding and transforms.
Raster conversion appears in the JPEG 2000 profile but was not the dominant cost.
No new TIFF metadata cache, speculative neighboring-tile prefetch, or compositor
rewrite was justified by these captures.

## Reproducible starting state

- Existing checkout: `wsi-rs checkout`; no worktree, commit, push,
  or publication. All prior changes were retained.
- HEAD: `c4df0ffe0ec19fdef00543f9e41d4d7f08481705`.
- Starting `git diff HEAD --binary` SHA-256:
  `8a7d8be89c92032ef2145b269b6d69d8f654bc9c7552e0bc0930dec7365ee25f`.
  This includes staged and unstaged tracked changes, including the prior CZI and
  codec-adapter work. The initial status is saved with the patch in
  `target/panning-2026-09-04/`; untracked pre-existing files were not edited.
- Cargo.lock SHA-256:
  `45ffbc0fefda84f89cc4b0e3e1a62479b8848fe8efa126a2932794270c6d19dc`.
- wsi-rs 0.7.0; j2k family 0.10.0, jxr 0.1.1, jxr-native/core/math 0.1.0;
  dicom-object/core/parser 0.9.1. Versions came from the installed lockfile.
- Apple M4 Pro, 12 logical CPUs, 48 GiB RAM, macOS 26.5.2 (25F84),
  aarch64-apple-darwin; Rust 1.96.0, LLVM 22.1.2.
- Release builds, default features (empty feature set), CPU decode;
  `RAYON_NUM_THREADS=4` for both versions. No changes to codec worker budgets.
  Cache environment overrides were unset. Captures ran sequentially, with no
  builds from this task running during measured captures. Other desktop activity
  was uncontrolled.

`environment.json` in the capture directory records versions, cache environment,
source sizes, and SHA-256 hashes for all four slides and all six DICOM series
files. The public corpus entries are `ndpi-001`, `svs-001`, `svs-jp2k-001`, and
`dicom-jp2k-001` in `tests/fixtures/parity_corpus.public.toml`.

## Workloads and measurement limits

The existing `wsi-rs-perf` harness supplies a 128-read diagonal pan control and
its established dimensions/checksums. Its diagonal trace is not an adjacent
viewport sequence. `tests/panning_performance.rs` adds a native-reader matrix:

- Eight 512 × 512 viewports, moving horizontally by 128 pixels: 75% overlap.
- Level 0 and native level 2. NDPI level 2 is 4× reduced; SVS JPEG and DICOM
  are approximately 16× reduced; SVS JPEG 2000 is approximately 8× reduced.
- A deterministic tissue anchor selected outside timing from nine 128 × 128
  probes; a top-edge origin sequence; a bottom-right sequence crossing image
  boundaries; deterministic scattered viewports as a control.
- Three independent opens per workload/profile; immediately repeat each sequence
  for the warm phase. Setup probes use a separate reader. Cold means a newly
  opened reader, **not** cold filesystem caches. OS caches were not flushed.
- Default: 64 MiB shared source tiles, 32 MiB display tiles, 32 MiB private
  aggregate. Disabled: all three zero. Constrained: 1 MiB shared source tiles,
  no display cache, 512 KiB private aggregate. NDPI's private MCU share is
  1 MiB / 0 / 16 KiB respectively; capacity allocation is unchanged.
- SHA-256 includes dimensions and every U8 output byte in request order, outside
  timing. Each result must be 512 × 512. The harness asserts identical results
  across repetitions, cache configurations, and cold/warm phases. Cross-version
  comparison also checks request coordinates and result cardinality.
- Timings cover region reads, including planning, reads, decode and composition;
  open time is recorded separately. Hashing and tissue selection are excluded.
  `/usr/bin/time -l` records whole-process peak RSS, including setup, allocator
  retention and decoders; this is not a cache-occupancy measurement.

The first exploratory quarter-slide anchor was nearly blank in two files and
was replaced before the accepted baseline. `*-before.json` captures use that
old anchor and are excluded. The initial NDPI shim sampling run was exploratory,
not an accepted timing baseline. Sampling profiles include hash/setup work and
waiting threads, so raw stack counts are not treated as CPU percentages.

Top-edge views sample blank/background in the SVS and DICOM images, but the NDPI
sample has colored content there. It does not establish performance on a truly
sparse NDPI image. Synthetic sparse and malformed-input behavior remains covered
by the repository tests. There is no available JPEG 2000 NDPI sample. One CPU,
one sample per format/codec, and uncontrolled desktop activity limit generality.
Short eight-read sequences do not support reliable p99 claims.

## Measurements

The main matrix compares `*-baseline-repeat.json` with `*-final.json` (three
repetitions each). All **576** corresponding checksums and request sequences
match. Earlier matched captures and the final repeat have the same pixels.
The NDPI extended run adds **90** matching checksums; the existing harness adds
**24**. No before/after pixel difference was observed.

### NDPI native matrix

Median elapsed milliseconds per eight-view sequence. Throughput is useful RGB
output (6 MiB per sequence), not compressed-source throughput. Top-edge and
boundary rows are included to expose clipping/background behavior rather than
being counted as tissue speedups.

| Cache | Workload | Cold before → after (ms) | Warm before → after (ms) |
| --- | --- | ---: | ---: |

| default | pan_l0 | 6.88 → 6.97 | 0.78 → 0.50 |
| default | pan_l2 | 5.94 → 5.09 | 0.59 → 0.45 |
| default | background_l0 | 2.80 → 2.56 | 0.53 → 0.39 |
| default | background_l2 | 4.46 → 3.92 | 0.53 → 0.38 |
| default | boundary_l0 | 1.56 → 1.32 | 0.27 → 0.22 |
| default | boundary_l2 | 1.04 → 0.82 | 0.30 → 0.18 |
| default | scattered_l0 | 28.72 → 26.78 | 1.15 → 0.97 |
| default | scattered_l2 | 21.44 → 19.78 | 0.74 → 0.64 |
| disabled | pan_l0 | 73.38 → 62.39 | 73.85 → 62.31 |
| disabled | pan_l2 | 21.69 → 17.19 | 21.64 → 17.06 |
| disabled | background_l0 | 49.03 → 40.62 | 48.68 → 40.14 |
| disabled | background_l2 | 21.63 → 17.18 | 21.71 → 17.20 |
| disabled | boundary_l0 | 23.79 → 19.81 | 24.16 → 19.63 |
| disabled | boundary_l2 | 6.53 → 4.97 | 6.35 → 4.90 |
| disabled | scattered_l0 | 67.02 → 57.11 | 67.31 → 57.03 |
| disabled | scattered_l2 | 24.46 → 19.40 | 24.36 → 19.39 |
| small | pan_l0 | 73.30 → 29.69 | 75.10 → 29.66 |
| small | pan_l2 | 18.38 → 14.70 | 18.18 → 14.57 |
| small | background_l0 | 48.86 → 16.48 | 47.77 → 16.13 |
| small | background_l2 | 19.88 → 15.58 | 19.72 → 15.59 |
| small | boundary_l0 | 24.10 → 8.11 | 24.06 → 7.86 |
| small | boundary_l2 | 1.06 → 0.84 | 0.31 → 0.19 |
| small | scattered_l0 | 67.37 → 26.81 | 67.14 → 26.37 |
| small | scattered_l2 | 24.46 → 19.58 | 24.65 → 19.62 |

### Longer NDPI level-0 check

Fifteen repetitions, eight viewports each, using `ndpi-extended-baseline.json` and
`ndpi-extended-final.json`. This follow-up investigates the apparent default-cache
regression in earlier short runs.

| Cache | Phase | Median before → after (ms) | Speedup | RGB output before → after (MiB/s) |
| --- | --- | ---: | ---: | ---: |
| default | cold_reader | 6.235 → 6.205 | 1.00× | 962.3 → 967.0 |
| default | warm_revisit | 0.476 → 0.451 | 1.06× | 12598.5 → 13295.1 |
| disabled | cold_reader | 70.613 → 62.463 | 1.13× | 85.0 → 96.1 |
| disabled | warm_revisit | 70.384 → 62.253 | 1.13× | 85.2 → 96.4 |
| small | cold_reader | 70.337 → 29.627 | 2.37× | 85.3 → 202.5 |
| small | warm_revisit | 70.142 → 29.456 | 2.38× | 85.5 → 203.7 |

The constrained-cache level-0 improvement repeats across captures (about
2.3–2.5×). Disabled-cache level-0 reads improve about 12–18%; reduced-level
constrained reads improve about 20–25% in the short matrices. The default
level-0 extended cold range was 6.057–7.390 ms before and 6.108–7.073 ms after:
these overlap substantially, and the medians differ by less than 1%. No default
panning speedup or robust default regression is claimed.

Earlier short captures showed a 4–8% default level-0 slowdown. Repeating the final
binary, extending to 15 repetitions, and checking the independent diagonal
harness did not reproduce a meaningful slowdown. Unchanged DICOM/SVS captures
also show machine variation, including large outliers. This is why small changes
are not interpreted as improvements. The final normalized-table branch avoids
an unnecessary second classification scan; file-absolute real-slide performance
was not measured.

### Existing harness: diagonal controls and latency distributions

Three captures of 128 reads each, four request workers, 64 MiB configured shared
cache, `pan_trace_l0` / `pan_trace_l2`. Before/after order alternates on repetition
1. Columns show the median of each run's elapsed time, throughput, p50 and p95;
latency includes concurrent request execution. These workloads scatter across
large slides and should not be confused with adjacent viewport panning.

| Source | Level | Elapsed before → after (ms) | Output before → after (MiB/s) | p50 before → after (ms) | p95 before → after (ms) |
| --- | --- | ---: | ---: | ---: | ---: |
| ndpi | l0 | 64.09 → 64.24 | 499.29 → 498.16 | 1.30 → 1.32 | 2.55 → 2.61 |
| ndpi | l2 | 55.51 → 55.06 | 576.48 → 581.16 | 1.26 → 1.30 | 1.74 → 1.70 |
| svs-jpeg | l0 | 27.26 → 27.44 | 1174.01 → 1166.18 | 0.37 → 0.37 | 0.43 → 0.44 |
| svs-jpeg | l2 | 45.16 → 44.90 | 708.65 → 712.71 | 0.86 → 0.87 | 1.27 → 1.21 |
| svs-jp2k | l0 | 276.29 → 277.25 | 115.82 → 115.42 | 7.80 → 7.83 | 13.43 → 13.28 |
| svs-jp2k | l2 | 86.54 → 86.29 | 369.78 → 370.85 | 0.81 → 0.81 | 5.62 → 5.57 |
| dicom-jp2k | l0 | 291.20 → 271.24 | 109.89 → 117.98 | 3.92 → 3.88 | 8.28 → 8.18 |
| dicom-jp2k | l2 | 69.92 → 69.48 | 457.67 → 460.57 | 2.13 → 2.10 | 4.46 → 4.49 |

DICOM and SVS differences above are controls, not benefits of the NDPI change.
The full native matrices for these unchanged formats also match all output
checksums. In particular, no DICOM indexing improvement is claimed from a noisy
cold first-read result.

### Peak process memory

Whole native matrix peak RSS from `/usr/bin/time -l`, MiB. Includes the setup
reader, all cache profiles, allocator retention, and codec allocations.

| Source | Before | After |
| --- | ---: | ---: |
| ndpi | 93.1 | 86.0 |
| svs-jpeg | 55.1 | 51.2 |
| svs-jp2k | 59.8 | 61.5 |
| dicom-jp2k | 66.9 | 57.5 |

No general RSS reduction is claimed: unchanged controls also vary. The actual
retained relative-offset entry shrinks from 953,664 bytes to 128 bytes for this
level, while the pre-existing TIFF tag allocation remains. Aggregate cache caps
are identical before and after.

## Profiling evidence and remaining costs

Profiles are retained as `*-profile.json.gz` and text summaries in the capture
directory. Symbols were resolved from the preserved binaries with `nm`; no codec
code was modified. The initial native NDPI profile contained 759 leaf samples in
`ndpi_jpeg_tile_payload` (including its inlined offset preparation), alongside
substantial platform copy work. Source inspection identified the full-table
`to_vec()` and classification loop on each cache miss. The regression test then
proved the duplicate allocation by failing an allocation-identity assertion.

After the change, disabled-cache classification scans remain visible, as expected.
Strips still require entropy decode, and undersized source-tile caches still
cause repeated decoding across viewports. File-absolute/high-word arrays retain
the original normalized-table path; the real corpus here does not establish a
speedup for that representation. Broader NDPI restart layouts, full-level decode
fallback performance, and other hardware remain unmeasured.

For SVS JPEG, the largest codec leaf was `decode_mcu_row` (753 samples). SVS JPEG
2000 had 4,310 cleanup-pass and 3,443 significance-pass samples; DICOM JPEG 2000
had 2,051 and 1,704 respectively. These profiles include all three cache profiles,
setup/hash work and waiting threads. Counts identify hot code; they are not
normalized cross-format performance comparisons. No measured evidence warranted
moving codec algorithms into this reader crate.

## Reproduction

From the existing checkout, build first and wait for completion:

```sh
cargo build --locked --release -p wsi-rs-perf -p wsi-rs-openslide-shim
cargo test --locked --release --test panning_performance --no-run
```

Use the test executable printed by the second command (on this capture it was
`target/release/deps/panning_performance-657baa5aa58623b9`). For each source:

```sh
RAYON_NUM_THREADS=4 \
WSI_RS_PAN_PATH=/path/to/parity-corpus/ndpi-001.ndpi \
WSI_RS_PAN_OUTPUT=/tmp/ndpi-panning.json \
/usr/bin/time -l target/release/deps/panning_performance-657baa5aa58623b9 \
  --ignored --nocapture
```

Other captured paths under the same corpus directory are `svs-001.svs`,
`svs-jp2k-001.svs`, and `dicom-jp2k-001.d/DCM_0.dcm`. Keep the complete DICOM
series directory. Leave cache environment overrides unset. The native test runs
all three cache profiles, both levels, four workloads, and both phases. For the
longer check, additionally set `WSI_RS_PAN_REPEATS=15` and
`WSI_RS_PAN_ONLY=pan_l0`.

The pre-change native reader and shim are preserved at
`target/panning-2026-09-04/baseline-native` and `baseline.dylib`; use them in the
same commands to reproduce the baseline without changing the working checkout.
`binary-hashes.json` records the exact executables/libraries used. The runner
scripts and full logs are retained in that directory, which is local ignored
benchmark output rather than committed source.

Existing harness control, repeated for `pan_trace_l2` and each corpus:

```sh
RAYON_NUM_THREADS=4 target/release/wsi-rs-perf \
  --engine wsi_rs \
  --library target/release/libwsi_rs_openslide_shim.dylib \
  --slide /path/to/parity-corpus/ndpi-001.ndpi \
  --workers 4 --cache-bytes 67108864 --only pan_trace_l0 --repeat-index 0
```

Use `baseline.dylib` for the before side. Repeat with indices 0, 1, and 2,
alternating which version runs first. The harness reports full output checksums,
latency samples, elapsed time, and throughput. For sampling, prefix the native
executable or harness invocation with `samply record --save-only -o profile.json.gz`.
Do not run builds during captures.

Native JSON comparison must key rows by cache/workload/phase/repeat and require
identical request coordinates, dimensions/level metadata and checksums before
comparing timing. The captured comparison is
`target/panning-2026-09-04/comparison-baseline-repeat-final.json`. Raw latency
vectors, open times, chromatic-pixel counts and viewport coordinates are retained
in each native capture. The eight-read totals and rates in this report are
medians across matching repetitions, not selected best runs.

## Verification

| Command | Result |
| --- | --- |
| `cargo test --locked --lib relative_mcu_starts_borrow` | Observed failing before the fix: relative offsets were copied rather than sharing the immutable TIFF allocation. |
| `cargo test --locked --lib formats::tiff_family` | 311 tests passed after the initial fix. |
| `cargo test --locked --lib mcu_` | 11 tests passed after adding marker accounting, eviction and normalization coverage. |
| `cargo xtask validate` | Formatting, Clippy, benchmark builds, docs and doctests passed. Default / OpenSlide-parity / Metal-parity: 987 / 994 / 1,023 passed, with 16 / 17 / 18 skipped. Includes the final NDPI implementation and all added tests. |
| `cargo xtask feature-check` | All 39 checks passed. |
| `cargo xtask coverage` | Workspace gates passed: 85.04% lines, 80.12% functions. |
| `cargo xtask coverage-changed --base HEAD --lcov lcov.info --threshold 80` | Initial default-only LCOV failed because pre-existing `src/output/download.rs` was absent. NDPI changed production lines were 100% in `ndpi_core.rs`, 88.89% in `caches.rs`. |
| `cargo llvm-cov --no-clean --workspace --all-targets --features parity-metal --lcov --output-path lcov.info --locked` | Passed and covered the pre-existing feature-gated download module. |
| `cargo xtask coverage-changed --base HEAD --lcov lcov.info --threshold 80` | Passed after Metal coverage: 89.67% (1,902/2,121). This gate measures the entire dirty checkout, including prior work. |
| `cargo xtask fuzz-check` | All nine targets built. No new mutation campaign was run. |
| `WSI_RS_PARITY_ALIASES=ndpi-001,svs-001,svs-jp2k-001,dicom-jp2k-001 cargo test --locked --features parity-openslide --test openslide_parity -- --ignored --nocapture` | Passed: 13 probes, no missing slides or failures; ten unsupported independent-reference probes. Supported SVS and NDPI OpenSlide comparisons had max/mean error zero. |
| `WSI_RS_PARITY_ALIASES=dicom-jp2k-001 cargo test --locked --test dicom_parity dicom_public_corpus_decodes_with_wsi_rs -- --ignored --nocapture` | Passed; three levels decoded. |
| `cargo test --locked --test real_wsi_behavior aperio -- --ignored --nocapture` | Four tests passed: JPEG/JP2K batch-vs-sequential equivalence, viewport cache reuse, raw JP2K passthrough. |
| Native release matrix and extended run | Passed; 576 + 90 matching before/after checksums. |
| Existing release harness | 24 matching before/after checksums. |
| `git diff --check` | Passed. Final diff/status reviewed; prior changes preserved. |

The independent JPEG reference differs from both wsi-rs and OpenSlide for the
three SVS JPEG probes; the existing test explicitly adjudicates those samples
against OpenSlide, where the results are exact. No tolerances or gates were
changed. DICOM JPEG 2000's declared RGB/OpenSlide color-conversion divergence is
recorded in the existing corpus manifest, so it is not claimed as an OpenSlide
pixel match. Native before/after equivalence is exact for that corpus.

Added regression coverage checks immutable allocation reuse under concurrent
access, disabled/undersized caches, relative and normalized revisits, nonzero
marker accounting, and mixed-marker/table eviction. Existing tests cover malformed
MCU spans, high words, file-absolute offsets, fallback, clipping, multiple strip
rows/columns, sparse tiles, source replacement, ordering and cancellation.

The task's source edits are confined to NDPI offset acquisition and its cache,
their regression tests, the native performance test, and these architecture/audit
documents. DICOM, SVS, external codecs, dependency manifests and lockfiles were
not changed by this task.
