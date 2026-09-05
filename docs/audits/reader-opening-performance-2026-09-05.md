# VMS, VSI and CZI opening performance — 2026-09-05

## Results

Three small changes remove measured opening overhead: bounded sequential ETS
index I/O, the existing SHA-256 hardware backend on Apple Silicon, and avoiding
unused embedded-CZI destination composition during associated-image probing.
The saved corpus includes the real VMS CMU-1 sample and Olympus OS-2 VSI with
its ETS companions. No codec algorithm was added to wsi-rs.

Matched release measurements on one real sample per format:

| Sample | Median open before → after | Median speedup | p95 before → after | Opens/s before → after |
| --- | ---: | ---: | ---: | ---: |
| VMS CMU-1, JPEG | 27.235 → 11.465 ms | 2.38× | 29.045 → 14.780 ms | 36.8 → 85.7 |
| VSI OS-2, JPEG 2000 ETS | 41.123 → 0.840 ms | 48.96× | 42.729 → 0.974 ms | 24.3 → 1,171.5 |
| CZI Zeiss-5-JXR | 8.170 → 5.727 ms | 1.43× | 10.543 → 8.899 ms | 115.8 → 163.7 |

Each side has three process runs of ten fresh-reader opens. The table pools
30 individual latencies for the median and nearest-rank p95. Opens/s is ten
divided by the median run's summed opening time, excluding close/setup. Run
order alternated before/after, after/before, before/after. These are **cold
reader caches with warm or uncontrolled filesystem caches**, not cold-storage
measurements. No builds from this task ran during accepted benchmark captures.
All opening workload checksums, dimensions, bounds and sample counts match.

| Sample | Observed open range before → after | Process peak RSS range before → after |
| --- | ---: | ---: |
| VMS | 26.759–31.142 → 11.210–17.635 ms | 31.16–31.20 → 31.27–39.56 MiB |
| VSI | 40.571–43.140 → 0.797–0.975 ms | 13.47–14.98 → 13.61–13.63 MiB |
| CZI | 7.553–11.477 → 5.163–13.302 ms | 37.11–38.06 → 31.55–32.48 MiB |

RSS comes from `/usr/bin/time -l` around the whole worker, including library
loading, source hashing, setup and allocator retention. One VMS after-run has
higher RSS; no VMS memory reduction is claimed. CZI has a slower after maximum
despite its lower median and p95. Desktop activity and filesystem cache state
were uncontrolled. Thirty samples from three processes do not justify p99 or
universal tail-latency claims. These measurements establish opening gains for
these samples, not panning gains or overall superiority to OpenSlide.

## Profile evidence and changes

### VMS

The preceding real-corpus survey recorded 235/315 samples inside
`Quickhash1::hash_file_part`, with 61 in `.opt` parsing. The installed
`sha2 0.10.9` selected its software path on aarch64 because `asm` was disabled.
The hash inputs and file-read policy were already correct; replacing quickhash,
omitting input bytes or deferring identity calculation would change semantics.

`Cargo.toml` now enables the external crate's `asm` feature only for
macOS/aarch64. SHA-256 remains in RustCrypto, including CPU-feature detection
and the software fallback. Other target configurations keep their previous
feature selection. The lockfiles add only `sha2-asm 0.6.4` and its dependency
edge; `cc 1.2.67` was already locked. On aarch64, sha2 uses its own guarded Rust
intrinsics; enabling this feature also compiles the optional assembly package.
The macOS C toolchain is required at build time.

The exact package's selected source/build integration was reviewed and recorded
in `supply-chain/audits.toml` and `SUPPLY_CHAIN.md`. The initial Cargo Vet check
failed for the missing review; it passed after that review, without an exemption
or gate change. This is a scoped integration audit, not formal cryptographic
verification or a review of every architecture's assembly.

`src/core/hash/tests.rs` adds independent hashlib/OpenSSL vectors for lengths
0, 55, 56, 63, 64, 65 and 4,097, streaming chunk sizes 1, 17, 64 and 4,096, and
file hashing. They passed before feature activation, after activation, and with
`sha2/force-soft`. This performance-only dependency configuration has no intended
behavior change, so its test-first evidence is an independent oracle that passes
on both implementations, rather than an artificial failing pixel assertion.
All real-slide quickhash values and properties are unchanged.

### VSI/ETS

The initial survey's 100,000-record synthetic index exposed tiny reads and seeks.
Before implementation, the real OS-2 sample confirmed the same bottleneck:
466/496 profile samples included `EtsIndex::read`, 383 included `File::read`,
72 included `File::seek`, and 452 had kernel leaves. These inclusive counts
overlap. The 27 samples in benchmark source hashing were outside reader opening.
OS-2 has two four-dimensional ETS indexes, with 494 and 19,312 chunk records.

`src/formats/olympus_vsi/scene/index.rs` now reads records through one temporary
8 KiB standard `BufReader`. `seek_relative` consumes reserved padding without
discarding buffered bytes. The coordinate vector is reused across records and
remains bounded by the validated 16-dimension maximum. The complete index is
still validated and retained during open; duplicate rejection, payload bounds,
axis/level limits, aggregate index budget and tile addressing are unchanged.
The private reader accepts `Read + Seek` for observed-I/O tests. Its caller does
not use the file position afterward. The buffer is dropped after parsing and
does not become a decoded-tile cache.

The test-first 1,024-record fixture failed with **6,144 underlying reads**.
It now passes ceilings of eight reads and two seeks, with no read request larger
than 8 KiB, while checking every indexed offset/count. Additional cases cover
three-byte short reads, refill boundaries, truncation and duplicate coordinates.
The new tests live in `src/formats/olympus_vsi/scene/index/tests/io.rs`; the
existing parser/resource-limit tests also pass.

### CZI

In the preceding Zeiss-5-JXR profile, 48/92 opening-stack samples included
associated-image probing and 47 included decoding. Benchmark file-hash samples
were excluded. The reader composed embedded associated images and converted
the finished canvas to RGB/U16 merely to obtain their metadata during open.

`src/formats/zeiss/attachments.rs` shares the existing embedded-CZI read path
between metadata probing and rendering. Probing now omits destination canvas
allocation, blitting and final sample conversion. It **still reads, preflights
and decodes every selected source subblock**. Default-plane selection, mosaic
ordering, supported compression, typed metadata, malformed payload errors and
the full decoded-plane budget check remain in place. Rendering still performs
the same composition and conversion. JPEG attachment probing is unchanged.

This deliberately stops short of header-only probing: deferring compressed
payload validation would alter open-time error behavior. It also avoids adding
an associated-image cache. The test-first probe initially composed two pixels;
it now composes zero. Fixtures cover BGR24, BGR48 and BGRA32, nonzero origins,
exact native RGB/U16 pixels, truncated payloads, unsupported compression and
mixed pixel types. These tests are in
`src/formats/zeiss/tests/attachment_cases.rs`.

## Pixel and metadata equivalence

The preserved baseline and after release libraries were loaded through the
existing C ABI, with checked function signatures. For each of the three samples,
12 viewports cover level 0, level 2 and the last level: adjacent overlapping
256×256 requests, bottom/right clipping and negative origins. Each set is read
on a fresh reader, revisited warm, then read by four simultaneous callers on
the same handle. Cache profiles are shared/display budgets of 64/32 MiB,
1/0 MiB and 0/0 MiB; existing private-cache policy remains unchanged.

All **324 before/after viewport checksum pairs** match, including dimensions
and cold/warm/concurrent ordering. All 62 VMS, 48 VSI and 49 CZI properties,
including quickhash, and all pyramid dimensions/downsamples match in every
cache profile. VMS macro and CZI label/thumbnail pixels match in all three
profiles (nine associated-image checksum pairs). VSI exposes no associated
images for this sample.

The CZI macro is 16-bit. Both libraries return the same existing C ABI error:
`to_rgba() requires Uint8 data; use to_rgba_windowed() for Uint16/Float32`.
Its dimensions match, but this harness does not verify the real macro's native
U16 pixel bytes. Exact native U16 fixture assertions cover the changed
composition/conversion boundary. This limitation was recorded, not hidden by
dropping the associated image or changing a repository assertion.

OpenSlide 4.0.1 comparisons pass for the real VMS sample. The parity harness
reports three probes with no failures/missing slides; its level-0 comparison
has maximum and mean error zero. Independent non-OpenSlide reference paths are
unsupported for all three VMS probes. The broader existing environment-path
OpenSlide test also passes for VMS. It fails for VSI (`openslide_open returned
NULL`) and CZI (`JPEG XR compression is not supported`) in the pinned local
build. Therefore those two samples have before/after equivalence and native
regression coverage, but no successful OpenSlide pixel oracle in this run.

## Starting state and reproduction

- Existing dirty checkout, HEAD `c4df0ffe0ec19fdef00543f9e41d4d7f08481705`.
  All prior changes, including the unfinished VSI module split, were preserved.
  No worktree, commit, push or publication was performed.
- Starting tracked patch SHA-256:
  `5048e07398efa5f3a9a55ff9fd8384062a90ef9e67c3e9e0a56d90d66d8383e8`.
  Starting status, patch and pre-edit copies of the touched manifests/index/
  attachment reader are saved with the artifacts.
- Apple M4 Pro, 12 logical CPUs, 48 GiB RAM, macOS 26.5.2,
  Rust 1.96.0 (`ac68faa20`), wsi-rs 0.7.0, default CPU release features.
- Codecs unchanged: j2k/j2k-jpeg 0.10.0; jxr 0.1.1 with
  jxr-core/jxr-native/jxr-math 0.1.0; czi-rs 0.1.0; sha2 0.10.9.
- Opening benchmarks: one caller/worker, `RAYON_NUM_THREADS=1`,
  `WSI_RS_SHIM_JP2K_CPU_THREADS=1`, 64 MiB API tile cache and unchanged
  default display/private-cache policy. The four-caller run above is a separate
  correctness check, not the opening benchmark thread budget.
- Artifacts: `target/reader-opening-fixes-49zba2w0/`, including raw JSON/timing
  logs, profiles, `environment.json` with all source/companion hashes,
  `comparison.json`, `verify_pixels.py`, `pixel-equivalence.json` and validation
  logs. The preceding profiles are in `target/reader-io-survey-q9q50ypf/`.
- Preserved baseline library SHA-256:
  `ca3269e16016b79ae4dd2c201463e79672871d56b46e6d27bf59f8ece4114bb0`.
  After library SHA-256:
  `00163653db1619f7f02bfc22db2dc774fe9cc278f1dedea45b7697ca884efc4a`.

Local source paths (companions must remain beside their source):

```sh
vms_source="${WSI_RS_CORPUS_ROOT:?Set the local corpus directory}/vms-001.d/CMU-1-40x - 2010-01-12 13.24.05.vms"
vsi_source="${WSI_RS_VSI_CORPUS_ROOT:?Set the OS-2 directory}/OS-2.vsi"
czi_source="${WSI_RS_CZI_CORPUS_ROOT:?Set the CZI corpus directory}/Zeiss-5-JXR.czi"

cargo build --locked --release -p wsi-rs-perf -p wsi-rs-openslide-shim
opening_artifacts="$PWD/target/reader-opening-fixes-49zba2w0"
RAYON_NUM_THREADS=1 WSI_RS_SHIM_JP2K_CPU_THREADS=1 \
/usr/bin/time -l target/release/wsi-rs-perf \
  --engine wsi_rs --library "$opening_artifacts/after.dylib" \
  --slide "$vms_source" --workers 1 --cache-bytes 67108864 \
  --only open_latency --repeat-index 0
python3 "$opening_artifacts/verify_pixels.py"
```

Repeat for all three sources and round indices 0, 1 and 2, alternating library
order in round 1. Use `baseline.dylib` for before and `after.dylib` for the
measured after state; a fresh build is at
`target/release/libwsi_rs_openslide_shim.dylib`. Retain stdout JSON and time's
stderr separately. The harness's `effective_elapsed_us` sums ten opens; compare
`samples_us` for individual latency. Compare workload checksums before timing.
Profiling separately wrapped the open-only command with
`samply record --save-only -o <profile.json.gz>`.

## Validation

| Executed command/check | Result |
| --- | --- |
| `cargo test --locked --lib olympus_vsi -- --nocapture` | 14 passed after the expected I/O-count regression failure before buffering. |
| `cargo test --locked --lib core::hash -- --nocapture` | Nine passed before and after hardware activation. |
| `cargo test --locked --features sha2/force-soft --lib core::hash -- --nocapture` | Nine passed. |
| `cargo test --locked --lib formats::zeiss -- --nocapture` | 92 passed after the expected composition-count failure. |
| `cargo test --locked --lib embedded_attachment_probe -- --nocapture` | Both passed, including the final exact typed-pixel assertions. |
| `cargo xtask validate` | Passed, repeated after final test refinements: format, Clippy, release harness/shim builds, docs and three doctests. Default/OpenSlide-parity/Metal-parity: 1,009 / 1,016 / 1,043 passed, with 16 / 17 / 18 existing skips. |
| `cargo xtask feature-check` | All 39 checks passed. |
| `cargo xtask deps` | Cargo Deny, Machete and Vet passed. Vet: 26 fully audited and 140 existing exempted packages. No gate was weakened. |
| `cargo xtask coverage` | Workspace gates passed: 85.17% lines, 80.16% functions. VSI: 91.74% lines, 72.09% functions. CZI: 85.10% lines, 74.69% functions. |
| `cargo llvm-cov --no-clean --workspace --all-targets --features parity-metal --lcov --output-path lcov.info --locked` | Passed, including pre-existing feature-gated changes. |
| `cargo xtask coverage-changed --base HEAD --lcov lcov.info --threshold 80` | Passed: 90.55% (2,233/2,466 lines) across the entire dirty checkout. Changed-path VSI index coverage: 92.64%; CZI attachments: 92.86%. |
| `cargo xtask fuzz-check` | All nine targets built. No mutation campaign was run. |
| `WSI_RS_PARITY_ALIASES=vms-001 cargo test --locked --features parity-openslide --test openslide_parity -- --ignored --nocapture` | Passed; reference limitations described above. |
| `WSI_RS_OPENSLIDE_COMPARE_PATHS="$vms_source" OPENSLIDE_LIB_PATH="${WSI_RS_OPENSLIDE_LIBRARY:?Set the pinned OpenSlide library}" cargo test --locked --test openslide_compare compare_against_openslide_for_env_paths -- --ignored --nocapture` | Passed for VMS. The same command with `$vsi_source` or `$czi_source` exits 101 with the OpenSlide open errors above. The initial combined three-path run also exited 101. |
| Preserved-library equivalence script | 324 viewport and nine readable associated-image checksum pairs match; properties and dimensions match. |
| `git diff --check` and final diff/status review | Passed. Task changes were compared with the saved starting patch and the pre-edit untracked VSI index. Concurrent checkout-action version edits in five CI workflows were left untouched. |

No language server was started or stopped by this task; the final
`lsp server list` reported no running servers. The performance delta and final
status are saved as `task-delta.patch` and `final-status.txt` with the artifacts.

Remaining bottlenecks include VMS `.opt` parsing and file I/O, VSI index
allocation/validation after syscall reduction, and CZI source decoding and
temporary embedded-file handling. No further refactor is justified by these
captures alone. There is no cold-filesystem/network-storage measurement, no
cross-machine or non-macOS hardware-hash measurement, and only one real sample
per format. Larger/more fragmented indexes and different associated-image
layouts need separate measurement. Cache eviction policy, public APIs, decode
thread budgets and codec implementations were not changed.
