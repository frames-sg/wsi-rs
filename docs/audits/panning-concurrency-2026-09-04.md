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

Final matched captures and validation results are recorded below after completion.

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
