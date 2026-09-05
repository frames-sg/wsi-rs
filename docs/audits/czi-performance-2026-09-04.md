# CZI source-block reuse — 2026-09-04

## Result and scope

The CZI reader reuses decoded JPEG/JPEG XR source blocks across neighboring
output tiles, composes 8-bit RGB directly, and retains one file handle for
subblock preflight. Decoding remains in the external `j2k` and `jxr` crates.
There are no public API or dependency changes in this optimization.

Median elapsed time for each 16-request workload, using default caches:

| Corpus | Workload | Before (ms) | After (ms) | Speedup |
| --- | --- | ---: | ---: | ---: |
| Zeiss-5-JXR | Level-0 pan | 2031.26 | 779.61 | 2.61× |
| Zeiss-5-Cropped | Level-0 pan | 2027.32 | 797.01 | 2.54× |
| Zeiss-5-JXR | Scattered level-0 tiles | 1663.63 | 1580.42 | 1.05× |
| Zeiss-5-Cropped | Scattered level-0 tiles | 1630.75 | 1595.56 | 1.02× |
| Zeiss-5-JXR | Level-2 batch | 578.59 | 87.50 | 6.61× |
| Zeiss-5-Cropped | Level-2 batch | 536.17 | 83.89 | 6.39× |

All 108 before/after result checksums matched across both corpora, all cache
profiles, all workloads, and cold/warm phases. A repeat of the optimized JXR
matrix matched another 54 checksums, with 2.59× pan and 6.77× batch speedups.
These measurements apply to these CZI workloads, not to every WSI format.

## Design and memory tradeoffs

The public sample's full-resolution source blocks contain 2056 × 2464 RGB
pixels (about 14.5 MiB), substantially larger than 256 × 256 output tiles.
An initial equal split among four private caches gave source blocks only
8 MiB, so none of these full-resolution blocks could be retained. That version
improved lower-resolution batches but barely affected level-0 panning.

The final allocation reserves half of the existing CZI private-cache budget
for decoded source blocks: 16 MiB of the default 32 MiB. The remaining half is
split among composed output tiles, whole levels, and associated images. The
shared source/display cache budgets are unchanged. The internal budget
allocator supports reserving multiple shares; its existing one-share callers
retain their prior allocations.

The source cache uses the existing byte-weighted LRU, including per-entry
accounting. Oversized blocks decode without retention; disabling caches still
works. The other CZI private caches have less capacity than before, so workloads
that repeatedly revisit large associated images or whole levels may retain
fewer results. A block larger than 16 MiB cannot benefit from default source
reuse. Alternating between large blocks can also cause eviction.

No cache or CZI seek lock is held during decoding. Concurrent misses may still
decode the same block independently; this change does not introduce a global
decode lock or new parallel execution machinery. Source identity is checked
before returning a cached subblock. Misses retain the existing span and input
limit checks through the reused preflight handle. This retains one additional
file descriptor for each open CZI slide, which closes with the slide.

The ordinary 8-bit WSI path consumes codec RGB directly and copies clipped rows
into RGB output. Uncompressed input keeps its direct clipped conversion path.
The BGR bitmap adapter remains for typed embedded CZI attachments. Mosaic order
is unchanged: M index followed by file position.

The cache cap is a retained-memory limit, not a process RSS limit. Whole-matrix
peak RSS was approximately 1.30–1.39 GiB and includes decoder allocations and
allocator retention. No reduction in peak process memory is claimed.

## Method and reproducibility

- Hardware: Apple M4 Pro; macOS 26.5.2; Rust 1.96.0.
- Release build, default features; wsi-rs 0.7.0 working tree, jxr 0.1.1,
  j2k 0.10.0. No codec implementation was changed between captures.
- Baseline HEAD: `c4df0ffe0ec19fdef00543f9e41d4d7f08481705`, with the preceding
  architecture/JPEG XR audit changes still uncommitted.
- Baseline working patch SHA-256:
  `44fd3732144f61c359c7573dee4d6146c73cb4c9c75e78f680dfe39f4144e04e`.
- Three repetitions per workload/cache profile. Every cold phase opens a new
  slide; a warm phase immediately repeats its requests. The OS file cache was
  not flushed. These are not cold-disk measurements.
- Profiles: defaults; all caches disabled; and a 1 MiB shared tile cache with
  display caching disabled. Private-cache capacity follows the existing policy
  of half the explicitly configured shared tile budget.
- Panning reads a 4 × 4 tile grid starting at a tissue block's center. Scattered
  reads use 16 distinct source-block centers in deterministic shuffled order.
  Batches use a 4 × 4 grid at native level 2.
- SHA-256 covers every output tile's dimensions and every pixel byte, outside
  the timed interval. Tests also require nonblank tissue and correct result
  cardinality. Results are compared across captures by profile, workload,
  phase, and repetition.

The corpora are CC0 samples from the
[OpenSlide Zeiss corpus](https://openslide.cs.cmu.edu/download/openslide-testdata/Zeiss/):

| File | SHA-256 |
| --- | --- |
| Zeiss-5-JXR.czi | `c202ddf7b0bd473cdbe29977aee07c10c207077779485c0b1f876e8c00da77f7` |
| Zeiss-5-Cropped.czi | `6defce5e5507f07d91ecdaa8f2c156026495c8cef9c4ab181fc3f6564629829f` |

Run the native benchmark once for each corpus:

```sh
WSI_RS_CZI_JXR_PATH=/path/to/Zeiss-5-JXR.czi \
WSI_RS_CZI_PERF_OUTPUT=/tmp/czi-performance.json \
cargo test --locked --release --test czi_performance -- --ignored --nocapture
```

Local raw captures and `comparison.json` are in `target/czi-optimization/`.
The preserved `baseline-benchmark` executable runs the pre-change reader; the
final reader was measured using the release test executable. The baseline used
for the table is `before-jxr-quiet.json` / `before-cropped.json`; optimized
captures are `after-jxr-final.json` / `after-cropped.json`. No builds from this
task ran during those captures; other machine activity was uncontrolled.
An earlier exploratory baseline overlapped a build and
is excluded from the table. The repeat is `after-jxr-repeat.json`.

## Variation and coverage limits

Cache-disabled cold workloads improved about 1–9% in the two main captures.
These small differences are less robust than the source-reuse gains. One
small-cache JXR level-2 batch capture was 7.5% slower than baseline; repeating
the complete optimized matrix gave 529.92 ms versus the 538.67 ms baseline
(about 2% faster). The cropped small-cache batch also changed by only about 2%.
Do not infer a reliable small-cache batch speedup from these measurements.

Three repeats and 16 requests per workload are insufficient for reliable p99
claims. The sample set covers two brightfield CZI files on one Apple CPU;
full-corpus throughput, other hardware, and associated-image performance were
not benchmarked. Existing uncompressed and typed-attachment behavior is covered
by tests, not by a new performance claim.

Regression tests verify one decode for neighboring compressed tiles, unchanged
pixels with disabled/undersized caches, byte-bounded eviction, aggregate cache
capacity, source replacement after a cache hit, mixed JPEG XR/uncompressed
overlap ordering, edge cropping, and public region reads across output tiles.

## Verification

| Command | Result |
| --- | --- |
| `cargo test --locked --lib subblock_cache` | Six regression tests passed. The shared-decode and full-resolution-budget assertions were first observed failing before their fixes. |
| `cargo test --locked --lib core::cache` | 14 cache tests passed, including weighted budgets at zero, odd, and maximum sizes. |
| `cargo xtask validate` | Formatting, Clippy, release benchmark builds, docs, and doctests passed. Default/OpenSlide-parity/Metal-parity test configurations passed 984/991/1020 tests, with 15/16/17 skipped respectively. |
| `cargo xtask feature-check` | All 39 feature checks passed. |
| `cargo xtask coverage` | Workspace gates passed: 85.01% line coverage and 80.16% function coverage. |
| `cargo llvm-cov --no-clean --workspace --all-targets --features parity-metal --lcov --output-path lcov.info --locked` | Passed; added coverage for the pre-existing feature-gated GPU download module. |
| `cargo xtask coverage-changed --base HEAD --lcov lcov.info --threshold 80` | Passed after the Metal run: 89.51% (1861/2079). This measures the whole working diff, including prior audit and concurrent changes. The initial default-only report failed because `src/output/download.rs` was absent from LCOV. |
| `cargo xtask fuzz-check` | All nine existing fuzz targets built; no new mutation campaign was run. |
| `cargo test --locked --test zeiss_czi real_jpegxr -- --ignored` | Passed with each corpus path separately, including tile, batch, region, and associated-image reads. |
| `git diff --check` | Passed. |

Checks were run in the existing checkout. Other workspace changes were
preserved. No commit, push, or publication is part of this optimization.
