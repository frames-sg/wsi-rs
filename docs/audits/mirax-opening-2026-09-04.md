# MIRAX opening: bounded index I/O — 2026-09-04

Final validation completed on 2026-09-05.

## Result

MIRAX hierarchy parsing now uses an 8 KiB `BufReader` around its existing index
file. Each record previously issued four separate four-byte file reads. All
records, pointer checks, page-cycle checks, index budgets, geometry calculations,
quickhash inputs and associated-image metadata are still processed during open.

On the available `mirax-001` / CMU-1 sample, 30 opens per implementation gave:

| Implementation | Median per open | p95 per open | Observed range |
| --- | ---: | ---: | ---: |
| Starting wsi-rs checkout | 68.225 ms | 79.180 ms | 66.675–83.503 ms |
| Buffered wsi-rs checkout | 5.873 ms | 6.730 ms | 5.400–6.909 ms |
| OpenSlide 4.0.1 | 9.924 ms | 10.998 ms | 9.490–11.186 ms |

The change is **11.6× faster than the starting reader** for opening this sample.
The buffered reader is about **1.69× faster than OpenSlide** for this measured
opening workload. This is a one-sample, one-machine opening comparison, not an
overall MIRAX or OpenSlide superiority claim. The harness's `open_latency`
elapsed field sums ten opens; the table above reports individual-open samples.

## Evidence and implementation

Before editing, three alternating open-only captures reproduced the problem:
median 65.051 ms for wsi-rs and 9.564 ms for OpenSlide. Their level/bounds metadata
checksums matched. A separate sampling capture had 759 samples, with 669 under
`mirax::helpers::read_i32_le`, 663 under `File::read`, and 723 under hierarchy
index parsing. These are inclusive, overlapping counts, not additive timings.
Only 13 leaf samples were in SHA-256. Small index reads were the justified first
target; this profile did not justify changing quickhash semantics or lazy loading.

The production change is confined to:

- `src/formats/mirax/index.rs`: wrap hierarchy index I/O in a bounded standard
  buffer. The buffer handles absolute page seeks and is dropped after index
  traversal. It does not retain the entire index file or survive reader opening.
- `src/formats/mirax/helpers.rs`: the two integer readers accept `Read`, allowing
  both ordinary files and the buffered stream without changing integer/error
  semantics. The existing private index context accepts `Read + Seek`, enabling
  a counting stream in regression tests; no public API or dependency changed.
- `src/formats/mirax/index/tests/io.rs`: exercise the complete hierarchy traversal
  with observed I/O, duplicate coordinates, record ordering, multiple levels,
  short reads, backward seeks, buffer-boundary crossing and truncation.

The new regression initially failed with **4,106 underlying reads for 1,024
records**. After buffering, it passes a ceiling of 16 underlying reads, with no
read request larger than 8 KiB. Existing MIRAX malformed-input and budget tests
remain unchanged. No decoder, codec algorithm, thread count, cache capacity,
parser validation or retained-index limit changed. Opening adds one temporary
8 KiB I/O buffer; this is not a new decoded-image cache.

## Matched workload verification

Three alternating rounds ran all eleven existing performance-runner workloads
on each implementation. All **33 before/after workload checksums** matched,
including metadata, both pan levels, warm revisits, cache pressure, zoom,
thumbnail, viewport, large region and batch export. Before/after dimensions,
bounds, sample counts and output-byte counts also matched.
An additional C ABI metadata snapshot compared all 61 exposed properties
(including quickhash) and all three associated-image dimensions; they match
exactly between the preserved and buffered libraries.

Selected median workload elapsed times (not individual request latency):

| Workload | Before ms | After ms |
| --- | ---: | ---: |
| Level-0 pan, 128 requests | 132.416 | 133.863 |
| Level-2 pan, 128 requests | 149.309 | 150.155 |
| Level-2 viewport | 2,230.515 | 2,228.871 |
| Warm level-0 revisit | 70.335 | 71.577 |
| Large level-0 region | 655.130 | 657.642 |
| Batch export | 597.564 | 597.817 |

These small differences do not establish a pixel-read speedup or regression.
The optimization removes opening I/O overhead; it does not defer index work to
the first viewport. Other desktop activity and filesystem caches were
uncontrolled. Cold reader opens are **not cold filesystem measurements**.

The OpenSlide comparison matched 21/33 workload hashes. Existing differences
remain in level-2 pan, level-2 viewport, zoom and thumbnail. Therefore only the
matching opening metadata is used for the OpenSlide speed claim above; the
pixel workloads establish exact before/after equivalence within wsi-rs.

Whole-process peak RSS across the full workload runs was 183.0–196.0 MiB before,
194.8–205.9 MiB after, and 151.1–158.7 MiB for OpenSlide. These maxima include
pixel caches, decoding, setup and allocator retention. They are not opening-only
memory measurements, and no memory reduction is claimed. Both engines were
configured with a 64 MiB API tile cache; wsi-rs retains its existing additional
private/display cache policy. This is not a claim of equal total process memory.

## Reproduction and starting state

- Existing checkout, HEAD `c4df0ffe0ec19fdef00543f9e41d4d7f08481705`, with all
  previous changes retained. No worktree, commit, push or publication.
- Starting tracked patch SHA-256:
  `ca41ab06e89872fcc230c1f5d689b015aa8d9bf8a3e2b8eb736f5451f0453571`.
- Apple M4 Pro, 12 logical CPUs, 48 GiB RAM, macOS 26.5.2; Rust 1.96.0.
  wsi-rs 0.7.0, j2k 0.10.0, default features, release build, CPU execution.
- One client handle/worker, `RAYON_NUM_THREADS=1`,
  `WSI_RS_SHIM_JP2K_CPU_THREADS=1`, 64 MiB configured API tile cache.
- Corpus: `mirax-001.d/CMU-1.mrxs` and its companion directory, 26 files totaling
  565,102,675 bytes. Per-file hashes and binary hashes are in `environment.json`.
- Artifacts are in `target/mirax-open-rrmt53wc/`: preserved `baseline.dylib`,
  starting patch/status, profiles, raw JSON/logs, comparison and validation logs.
  No builds from this task ran concurrently with measured captures.

```sh
cargo build --locked --release -p wsi-rs-perf -p wsi-rs-openslide-shim

RAYON_NUM_THREADS=1 WSI_RS_SHIM_JP2K_CPU_THREADS=1 \
/usr/bin/time -l target/release/wsi-rs-perf \
  --engine wsi_rs \
  --library "$PWD/target/release/libwsi_rs_openslide_shim.dylib" \
  --slide "${WSI_RS_CORPUS_ROOT:?Set the local corpus directory}/mirax-001.d/CMU-1.mrxs" \
  --workers 1 --cache-bytes 67108864 --repeat-index 0
```

Add `--only open_latency` for the opening-only diagnostic. To reproduce the
baseline, use the preserved `baseline.dylib` as `--library`. For OpenSlide, use
`--engine openslide` and the installed pinned OpenSlide 4.0.1 library. Run three
rounds, reversing engine order in the middle round. Each full round yields ten
open samples per engine plus the ten pixel workloads. Compare matching workload
names, metadata and checksums before timing. Profiling used `samply record
--save-only -o open-profile.json.gz` around the opening-only command; it was
separate from accepted timing captures.

## Validation

| Command/check | Result |
| --- | --- |
| `cargo test --locked --lib hierarchy_index -- --nocapture` before buffering | Expected failure: 4,106 underlying reads exceeded the 16-read ceiling. Short-read/truncation test passed. |
| `cargo test --locked --lib mirax -- --nocapture` after buffering | All 31 tests passed. |
| `cargo xtask validate` | Formatting, Clippy, release harness/shim builds, docs and three doctests passed. Default/OpenSlide-parity/Metal-parity: 1,003 / 1,010 / 1,037 passed, with 16 / 17 / 18 existing skips. |
| `cargo xtask feature-check` | All 39 checks passed. |
| `cargo xtask coverage` | Workspace gates passed: 85.15% lines and 80.13% functions. MIRAX: 89.79% lines, 71.26% functions. |
| `cargo llvm-cov --no-clean --workspace --all-targets --features parity-metal --lcov --output-path lcov.info --locked` | Passed, covering pre-existing feature-gated work. |
| `cargo xtask coverage-changed --base HEAD --lcov lcov.info --threshold 80` | Passed: 90.55% (2,166/2,392), including the entire dirty checkout. Changed MIRAX production lines are 100% covered in both files. |
| `cargo xtask fuzz-check` | All nine targets built, including `open_mirax_bundle_bytes`. No mutation campaign was run by this task. |
| `WSI_RS_PARITY_ALIASES=mirax-001 cargo test --locked --features parity-openslide --test openslide_parity -- --ignored --nocapture` | Passed: three probes, no missing slides/failures. The level-0 OpenSlide comparison was exact. All three independent-reference paths were unsupported; higher-level pixel identity with OpenSlide is not claimed. |
| Matched release captures and metadata snapshots | 33/33 before/after workload hashes match; all 61 properties and three associated-image dimensions match. |
| `git diff --check` | Passed; final source diff/status reviewed. |

The task modified only the two MIRAX production files above, their new I/O
regression test, and this report. Concurrent unrelated CZI preflight and
supply-chain audit changes appeared during the run and were left untouched. Previous dirty work was
preserved. No language server was started or stopped by this task.

Remaining questions are opening behavior on larger/more fragmented MIRAX
indexes, network storage and cold filesystem caches. Repeated seeks between
small pages can reduce buffering's benefit. If further optimization is needed,
profile the buffered reader before changing allocation strategy or introducing
lazy indexes; the present evidence supports this small I/O change.
