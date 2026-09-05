# Bounded NDPI batching and shared region misses — 2026-09-04

## Changes and scope

This follow-up builds on the offset-table optimization in
`panning-performance-2026-09-04.md`. That improvement is included in this report's
baseline; speedups here are additional to it.

**NDPI:** integral region reads now decode small restart-strip batches using the
existing Rayon pool. Batch size is bounded by the region's existing staging
reservation, the established NDPI batch caps (four wide/eight narrow strips), and
the current pool size. Oversized strips still run alone. The first source tile
establishes output metadata and is released after its blit; each subsequent batch
is released before the next. The shared compositor retains clipping and output
ordering. NDPI region-batch errors are selected in request order after parallel work.
A private region-reader adapter confines this policy to the admitted fast path;
direct tile batches and synthetic-level cache loaders retain their previous policy.

**DICOM and SVS:** the shared CPU region compositor coalesces overlapping active
cache misses. A batch decodes and publishes the keys it owns before waiting for
keys owned by another batch, so intersecting requests do not form wait cycles.
Completed pixels are shared by `Arc`, with no second decoded-pixel cache. Failed
loads are retried through each caller's source path to preserve typed errors;
errors and panics release ownership. The coordinator bypasses waiting for Rayon
workers and reentrant owners. Disabled caches bypass it too.

The coordinator has at most `min(cache_bytes / 128, 128)` active producer records.
Its registry contains weak references; only active callers retain completion
records and pixels. This adds bounded coordination bookkeeping beyond the
existing pixel-payload accounting, not an increased decoded-cache capacity.
Ordinary LRU eviction remains byte-bounded. A waiter may still hold its decoded
result after LRU eviction, just as an existing active region could hold a tile.

No public API, dependency, codec algorithm, cache capacity, admission limit, or
configured thread budget changed. Explicit controlled tile APIs keep their
existing cancellation boundaries. The shared coordinator serves region/display
composition; it is not a claim that every direct tile API coalesces requests.
Codec region reconstruction and JPEG 2000 color conversion were not changed in
this pass.

## Why these changes

The preceding profile showed NDPI's streaming compositor calling `resolve_one`
for each strip and the NDPI batch fallback using a sequential iterator. A 512 ×
512 viewport spans many 2048 × 8 source strips, leaving the configured worker pool
underused. There was already room for several strips in the admitted staging
space: `Slide::region_work_bytes` reserves the larger of output RGBA bytes and
one source tile. The new planner divides this existing space rather than raising
the limit. The fallback remains serial when the region cannot fit two strips.

The concurrent baseline showed approximately 38–40 source-cache insertions for
18–21 distinct SVS/DICOM tiles, without eviction. Scattered reads had one insertion
per distinct tile. Synthetic regression tests then demonstrated two reads of each
shared tile in overlapping batches. The implementation reduces this to one read
per tile. Cache insertion counts establish redundant source work; they are not
claimed as direct hardware codec counters.

The initial bounded-batch regression failed with zero batch calls. The initial
NDPI fast-path regression failed because no region fast path existed. The initial
coalescing regression observed read counts `[1, 2, 2, 1]` for four distinct source
tiles; the corrected result is `[1, 1, 1, 1]`.

## Baseline and method

- Existing checkout `wsi-rs checkout`, HEAD
  `c4df0ffe0ec19fdef00543f9e41d4d7f08481705`, with all prior dirty work preserved.
  No worktree, commit, push or publication.
- Starting tracked patch SHA-256:
  `c18174210ef96e15476a875f6837f5c72f0e18b43b2852a61209dcc2ff0ceb60`.
  Patch/status and the preserved baseline executable are in
  `target/panning-concurrency/`. Previously untracked files were preserved; the
  existing panning test was extended in place.
- Apple M4 Pro, 12 logical CPUs, 48 GiB RAM; macOS 26.5.2; Rust 1.96.0,
  aarch64-apple-darwin. Release, default features, CPU decode.
- j2k family 0.10.0, jxr 0.1.1, dicom-object/core/parser 0.9.1. Lockfile and
  source hashes are recorded in the capture directory's `environment.json`.
- `RAYON_NUM_THREADS=4` on both versions. Sequential traces use one caller;
  concurrent traces use four caller threads on both sides, in two synchronized
  waves. No builds from this task ran during measurements; desktop activity and
  filesystem caches were uncontrolled.
- The existing native panning test covers eight 512 × 512 viewports with 75%
  horizontal overlap, native levels 0 and 2, a deterministic tissue anchor,
  top-edge content, clipped bottom-right boundaries, and scattered reads.
- Default caches: 64 MiB shared + 32 MiB display + 32 MiB private aggregate.
  Disabled: all zero. Constrained: 1 MiB shared + zero display + 512 KiB private.
  Configuration is identical before and after.
- Three independent reader opens per matrix row, followed by an immediate warm
  revisit. Cold means cold reader caches, **not** cold filesystem caches.
- Timings exclude hashing and setup. Concurrent elapsed time includes spawning
  and joining the caller threads; per-request latencies are also saved. Source
  cache counters are read outside the timed interval. The observation cache has
  exactly the same configured capacity as the cache it replaces before reading.
- Every output dimension and byte is hashed in original request order. Comparisons
  require identical metadata, request coordinates and SHA-256 before timing is
  compared. `/usr/bin/time -l` records whole-process peak RSS.

Corpora are the existing public `ndpi-001` (JPEG), `svs-001` (JPEG),
`svs-jp2k-001`, and `dicom-jp2k-001` entries. The JPEG DICOM and JPEG 2000 NDPI
corpus gaps and independent-reference limitations from the preceding report
still apply. The NDPI top edge contains colored content; it is not a proven
sparse-region benchmark.

## Final measurements

All times below are elapsed milliseconds for eight 512 × 512 RGB viewports
(6 MiB of output). Throughput counts output bytes, not compressed input bytes.
The longer traces have 15 reader opens and 120 observed request latencies per
cache/phase. Timings describe this machine under the recorded load; they are not
isolated-machine estimates or confidence intervals.

### NDPI: sequential level-0 panning

| Cache / phase | Before ms | After ms | Ratio | Output MiB/s before → after | Request p50 / p95 ms before → after |
| --- | ---: | ---: | ---: | ---: | --- |
| Default, cold reader | 6.897 | 3.948 | 1.75× | 870 → 1,520 | 0.072 / 4.250 → 0.075 / 1.987 |
| Default, warm revisit | 0.572 | 0.643 | 0.89× | 10,496 → 9,337 | 0.062 / 0.187 → 0.068 / 0.139 |
| Disabled, cold reader | 64.407 | 23.964 | 2.69× | 93 → 250 | 6.202 / 12.370 → 2.486 / 5.165 |
| Disabled, revisit | 64.434 | 24.086 | 2.68× | 93 → 249 | 6.730 / 12.597 → 2.447 / 4.739 |
| Constrained, cold reader | 31.486 | 15.819 | 1.99× | 191 → 379 | 3.140 / 6.253 → 1.656 / 3.195 |
| Constrained, revisit | 31.310 | 15.309 | 2.05× | 192 → 392 | 3.509 / 6.285 → 1.468 / 3.232 |

These are the final `ndpi-extended-serial-*` captures. Default cold source puts
remain 128 on both sides; constrained puts remain 704. This gain comes from
orchestration, not a larger cache or less requested output. The fully warm
sequence regresses by 0.071 ms (12%): the bounded batch resolver has more work
than the former single-tile cache-hit loop.

Two alternating 15-repeat level-2 controls confirmed gains after the initial
three-repeat matrix was noisy:

| Cache, cold reader | Pair 1 before → after ms | Pair 2 before → after ms |
| --- | ---: | ---: |
| Default | 6.011 → 3.937 | 6.647 → 3.652 |
| Disabled | 19.351 → 9.999 | 20.648 → 9.647 |
| Constrained | 16.476 → 10.081 | 17.451 → 8.420 |

The initial level-2 matrix included apparent default/disabled slowdowns
(5.937 → 6.423 and 19.195 → 21.708 ms). These results are retained; the longer
controls do not reproduce them. Level 2 uses 512 × 8 strips, versus 2048 × 8 at
level 0, so scheduling overhead matters more there.

### Concurrent level-0 panning

These 15-repeat traces use four callers and the same four-worker pool on both
versions. Each sequence contains two caller waves.

| Source, default cold reader | Before → after ms | Output MiB/s before → after | Request p50 / p95 ms before → after | Source puts before → after |
| --- | ---: | ---: | --- | ---: |
| NDPI JPEG | 5.422 → 3.706 | 1,107 → 1,619 | 2.469 / 7.408 → 2.958 / 12.939 | 303 → 265 |
| SVS JPEG | 4.219 → 4.911 | 1,422 → 1,222 | 1.772 / 2.731 → 1.777 / 3.373 | 36 → 21 |
| SVS JPEG 2000 | 54.136 → 49.955 | 111 → 120 | 25.364 / 35.287 → 27.710 / 35.000 | 38 → 18 |
| DICOM JPEG 2000 | 41.610 → 37.239 | 144 → 161 | 26.396 / 44.726 → 29.151 / 45.106 | 40 → 21 |

SVS/DICOM final entries equal final puts in these default-cache cases, with zero
evictions. Redundant source insertions are eliminated. NDPI retains its separate
source-strip cache; its shared-cache puts are not a count of unique codec calls.

The measured SVS JPEG 2000 and DICOM sequence gains are about 8% and 11% lower
elapsed time, respectively. Their median individual request latency does not
improve, and their p95 is approximately unchanged. NDPI concurrent p95 worsens
despite a faster sequence median. Sharing makes completion dependent on a
producer's scheduling; fewer decodes do not guarantee better latency tails.

### Conflicting timings and repeat controls

A process snapshot during investigation showed several unrelated CPU-heavy
processes (Python, a fuzz process, and Spotlight), in addition to the desktop.
Those sessions were left untouched. `load-control.txt` records the observation;
it is evidence of contention at that instant, not a continuous load trace.
Even disabled-cache controls, where coalescing is bypassed, moved substantially.

The initial SVS JPEG concurrent slowdown above was not stable. Three additional
alternating 15-repeat pairs gave:

| Pair | Before → after ms | Output MiB/s before → after | Request p95 before → after ms | Source puts before → after |
| --- | ---: | ---: | ---: | ---: |
| 1 | 4.632 → 3.957 | 1,295 → 1,516 | 3.450 → 2.595 | 35 → 21 |
| 2 | 4.448 → 3.899 | 1,349 → 1,539 | 3.537 → 3.515 | 36 → 21 |
| 3 | 5.168 → 4.311 | 1,161 → 1,392 | 3.936 → 2.931 | 36 → 21 |

These repeats show 12–17% lower elapsed time, but the conflicting initial result
precludes a universal SVS JPEG speed claim. The redundant-work reduction is
consistent. Constrained and disabled controls are retained in the raw captures.

The three-repeat SVS JPEG 2000 **serial** matrix had an apparent 64.048 →
100.044 ms regression. Warm time also jumped from 0.577 to 1.846 ms, and disabled
cold time from 254.840 to 332.404 ms. Three alternating 15-repeat serial controls
instead gave 62.929 → 62.067, 65.913 → 66.656, and 59.099 → 60.213 ms, all within
about 2%. No serial SVS/DICOM optimization is claimed.

The initial concurrent SVS JPEG 2000 level-2 row likewise showed 29.194 →
37.119 ms. Two alternating 15-repeat controls gave 25.726 → 22.150 and
25.598 → 22.154 ms. Disabled-cache controls were approximately unchanged
(49.008 → 48.021 and 48.675 → 48.541 ms); constrained controls gave
45.614 → 43.957 and 47.436 → 45.026 ms. The longer samples support a reduced-level
concurrent gain for this corpus; the initial outlier remains in the matrix.

### Boundaries, top edge and scattered controls

The complete three-repeat matrix also covers both native levels, all caches,
and cold/warm phases. Selected default-cache cold concurrent results follow;
these small samples support workload coverage, not precise tail estimates.

| Source | Top edge before → after ms | Boundary before → after ms | Scattered before → after ms |
| --- | ---: | ---: | ---: |
| NDPI JPEG | 4.729 → 2.287 | 2.446 → 1.347 | 17.665 → 11.241 |
| SVS JPEG | 1.266 → 1.074 | 0.866 → 0.618 | 5.380 → 4.563 |
| SVS JPEG 2000 | 12.447 → 13.328 | 2.544 → 2.333 | 95.497 → 94.496 |
| DICOM JPEG 2000 | 42.134 → 38.657 | 23.403 → 22.567 | 68.126 → 68.904 |

Scattered source puts are unchanged: NDPI 649, SVS 72, DICOM 75. The new shared
coordinator does not reduce independent source work. The small top-edge SVS JPEG
2000 slowdown and mixed constrained-cache results are not hidden. Sparse-tile
semantics are regression-tested; the real top-edge trace is not proof of sparse
storage in every sample.

### Peak process memory

| Source | Serial MiB before → after | Concurrent MiB before → after |
| --- | ---: | ---: |
| NDPI JPEG | 98.3 → 91.5 | 95.3 → 101.2 |
| SVS JPEG | 57.9 → 58.2 | 61.9 → 57.9 |
| SVS JPEG 2000 | 58.3 → 54.2 | 58.5 → 63.3 |
| DICOM JPEG 2000 | 59.4 → 56.3 | 66.0 → 61.8 |

These are process maxima for the complete matrices, including setup and all
cache profiles, not per-row cache residency. There is no general memory-reduction
claim. NDPI concurrent peak rises by 5.9 MiB and SVS JPEG 2000 by 4.8 MiB in these
captures. Configured cache capacities, admission limits and worker counts remain
identical. Active buffers, allocator retention and codec scratch affect RSS.

### Pixel equivalence and remaining profile

The main matrices and longer level-0 traces contain 1,602 exactly matching
before/after sequence hashes. The three-pair SVS controls add 540; NDPI's and
SVS JPEG 2000's level-2 controls add 180 each. The total is **2,502 matching
sequences, or 20,016 viewports**. Serial and concurrent final matrices also match
each other in original request order. Each hash covers dimensions and every
output byte. All cache profiles within each benchmark invocation agree as well.

A final NDPI sampling capture (`ndpi-final-profile.json.gz`) still shows external
JPEG entropy/color conversion and NDPI payload preparation. The latter includes
the intentionally uncached classification scan when caches are disabled. Of
8,176 samples, 470 leaf samples resolve to external RGB444 entropy decoding, 193
to external NEON color conversion, and 440 to NDPI payload preparation. The
profile includes setup, hashing and waiting threads; it is not a timing
breakdown of only viewport reads. Codec ROI/reconstruction remains external work.

## Reproduction

Build once, wait for completion, then run the executable printed by Cargo:

```sh
cargo test --locked --release --test panning_performance --no-run

RAYON_NUM_THREADS=4 \
WSI_RS_PAN_PATH=/path/to/parity-corpus/ndpi-001.ndpi \
WSI_RS_PAN_OUTPUT=/tmp/ndpi-serial.json \
/usr/bin/time -l target/release/deps/panning_performance-657baa5aa58623b9 \
  --ignored --nocapture
```

Use `WSI_RS_PAN_CONCURRENT=4` for concurrent viewport waves. For the extended
level-0 trace, add `WSI_RS_PAN_ONLY=pan_l0 WSI_RS_PAN_REPEATS=15`. Other source
paths under the same corpus directory are `svs-001.svs`, `svs-jp2k-001.svs`, and
`dicom-jp2k-001.d/DCM_0.dcm`. Keep the full DICOM series directory. Leave cache
configuration environment overrides unset.

For the reduced-level controls, set `WSI_RS_PAN_ONLY=pan_l2` and
`WSI_RS_PAN_REPEATS=15`: NDPI uses one caller, SVS JPEG 2000 four. Run two matched
pairs, reversing executable order in the second. For the three-pair level-0
controls use SVS JPEG with four callers or SVS JPEG 2000 with one, alternating
executable order. Raw names are `*-control-{0,1,2}-{before,after}.json` and
`*-level2-control-{0,1}-{before,after}.json`.

The preserved before executable is `target/panning-concurrency/baseline-native`.
Use it with the same environment to reproduce before measurements without
changing the working checkout. `paired.py` in that capture directory runs the
full matched matrices and longer traces, alternating before/after order across
corpora. `compare.py` checks metadata, coordinates and hashes before writing
`comparison.json`. The serial and concurrent traces have identical ordered
outputs; only execution differs.

The benchmark is an extension of the existing native panning test, not a new
production API. It records end-to-end sequence elapsed time, per-request latency,
source-cache insertions/entries/evictions, open time, dimensions, coordinates and
pixel hashes. Hashing and statistics snapshots are outside timing. Do not run
builds concurrently with captures.

## Validation

| Command | Result on final source |
| --- | --- |
| `cargo test --locked --lib ndpi` | 50 passed after scoping the parallel adapter to regions. |
| `cargo test --locked --lib coalescing` | Both overlapping-read and failed-load regressions passed. |
| `cargo xtask validate` | Formatting, Clippy, benchmark builds, docs and three doctests passed. Default / OpenSlide-parity / Metal-parity: 997 / 1,004 / 1,033 passed; 16 / 17 / 18 existing skips. |
| `cargo xtask feature-check` | All 39 checks passed. |
| `cargo xtask coverage` | Workspace gates passed: 85.10% lines (32,040/37,649), 80.04% functions (2,980/3,723). |
| `cargo llvm-cov --no-clean --workspace --all-targets --features parity-metal --lcov --output-path lcov.info --locked` | Passed; includes prior feature-gated work in the dirty checkout. |
| `cargo xtask coverage-changed --base HEAD --lcov lcov.info --threshold 80` | Passed: 90.23% (2,078/2,303) across 40 files. This measures the entire dirty checkout. New flight coordinator: 98.61%; changed region/resolver lines: 100% / 95.45%; NDPI batch file: 94.87%. |
| `cargo xtask fuzz-check` | All nine fuzz targets built. No new mutation campaign was run by this task. |
| `WSI_RS_PARITY_ALIASES=ndpi-001,svs-001,svs-jp2k-001,dicom-jp2k-001 cargo test --locked --features parity-openslide --test openslide_parity -- --ignored --nocapture` | Passed: 13 probes, zero missing slides/failures; ten independent-reference probes unsupported. Supported SVS/NDPI comparisons against OpenSlide were exact. |
| `cargo test --locked --release --test panning_performance --no-run` | Passed; completed before matched measurements started. |
| Matched release matrices and repeat controls | 2,502 sequence checksums matched; 20,016 viewports. |
| `git diff --check` and `cargo fmt --all -- --check` | Passed after the measurements. Final diff/status reviewed against the captured starting state; the lockfile remains identical. An unrelated concurrent `.gitignore` edit appeared during final review and was preserved. Other tracked source changes match the starting snapshot. |

The first changed-coverage attempt classified a new test file outside the
repository's conventional `tests/` directory as production, while LCOV excluded
its test code. Moving that unchanged test into `region/tests/coalescing.rs`
resolved the mismatch. Neither the test assertions nor the coverage gate changed.
The red-stage logs retain the expected redundant-read/batching failures.

Validation logs are retained as `*-final.log` in the capture directory. The
selected DICOM three-level and four Aperio real-slide integration checks also
passed in the preceding validation pass; the final native matrix and OpenSlide
preflight exercise the scoped implementation against the same real samples.

## Regression coverage and limitations

- Bounded streaming batches preserve pixels and result ordering; the existing
  one-at-a-time streaming test remains unchanged and passes.
- NDPI region tests compare the fast path to sequential composition across
  clipping, negative origins, right/bottom boundaries and cache profiles.
- NDPI region batches preserve duplicate requests and first-error ordering.
  Pre-cancelled controlled reads perform no source decode.
- Overlapping region batches read each distinct source tile once, preserve
  original result slots, and release failed work for typed-error retries.
- Coordinator tests cover disabled/tiny caches, its active-record bound, eviction
  while a waiter holds a result, failed producers, panic cleanup, reentrant
  callers and one-worker Rayon pools.
- Existing reader tests cover sparse tiles, source replacement, decode limits,
  cardinality, cancellation, typed composition and fractional origins.

The NDPI fast path applies to integral regions backed by restart strips. It does
not change direct-tile scheduling, whole-level cache loaders or fractional-region
streaming. The coordinator avoids waiting inside Rayon; applications issuing
region requests from Rayon workers will not get the same coalescing behavior as
these external caller threads. Disabled caches also bypass shared coalescing.

The first concurrency implementation reached generic NDPI tile batches. Final
review narrowed it to a private `NdpiRegionReader` adapter so synthetic-level
cache loaders and other callers retain their execution policy. All final tests
and accepted measurements use this scoped version.

No codec ROI implementation, JPEG 2000 arithmetic optimization, GPU performance
claim, or in-place color-conversion change is included. The real sample set is
small, OS caches and desktop activity are uncontrolled, and eight-read matrices
do not justify p99 claims. Extended traces supply 120 observed request latencies
per cache/phase; those requests are correlated within repeated pan sequences.
Peak RSS includes allocator/decoder state and setup, not just retained caches.

The independent-reference caveats are unchanged: the selected preflight compares
supported SVS/NDPI probes to OpenSlide. Three SVS JPEG probes disagree with the
independent JPEG reference; the established test adjudicates these against
OpenSlide, where wsi-rs matches exactly. Ten independent-oracle probes are
unsupported. The DICOM RGB JPEG 2000/OpenSlide conversion divergence is declared
in the existing corpus manifest and is not claimed as a pixel match against
OpenSlide. Exact before/after equivalence is measured separately.
