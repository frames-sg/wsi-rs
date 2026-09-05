# Release readiness and repository documentation refresh — 2026-09-05

## Release status

**wsi-rs 0.7.0 is prepared in PR #15 but is not release-ready. No merge, release
tag, or crates.io publication was performed.** The full local release gate
fails corpus coverage, and the fresh performance capture fails the NDPI zoom
workload under the existing memory limits. Passing ordinary CI does not waive
these release gates.

[Release PR](https://github.com/frames-sg/wsi-rs/pull/15).
The final tested code commit before this report is
`e777740e4f857c10bd6fea0b0e2ded488f26a426`. Subsequent report/example edits do not
change reader behavior. Work used the existing checkout; unrelated changes in
other repositories and their real Git indexes were preserved.

## Completed cleanup and verification

- Consolidated the 0.7 changelog and removed contradictory CZI support claims.
  Earlier releases and historical audit evidence remain identified as history.
- Updated JXR CPU dependency-audit documentation; no supply-chain exemption was
  added. Codec algorithms remain in the external J2K/JXR crates.
- Replaced personal filesystem paths in reproduction commands with environment
  variables. Excluded `docs/audits/**` from the crate archive while keeping
  public API/architecture documentation and licenses.
- Removed the obsolete `tolerant_regions` key from the private corpus example;
  the current manifest schema rejects that removed field.
- Pinned Gitleaks 8.30.1 found zero detections in 558 candidate source files,
  full local Git history, and the final release commit range. The downloaded
  scanner checksum was verified. Scans used full redaction and ignored inline
  allow directives; zero detections are evidence, not a guarantee of no secrets.
- The release identity check and publish-policy behavior tests passed. Package
  enumeration contained 420 files, with internal audit reports and build output
  excluded.
- All 17 ordinary PR checks passed at `e777740`, including coverage, supply
  chain, API checks, platform release builds, feature checks, docs, packaging,
  fuzz builds, and Gitleaks.
- [CUDA validation](https://github.com/frames-sg/wsi-rs/actions/runs/33953500402)
  passed on that exact commit using the organization runner `Cuda`.
- [All four C artifacts](https://github.com/frames-sg/wsi-rs/actions/runs/33953499342)
  passed architecture, identity, dependency, export and C ABI checks: Linux x64,
  Windows x64, macOS arm64 and macOS x64. The source-preflight jobs in that
  workflow are separate from those artifact results.

Two Windows tooling defects were reproduced and fixed. Git Bash selected its
own `link.exe` utility instead of the MSVC linker, so the artifact command now
uses the platform default shell. The PE dependency checker then misread
`dumpbin`'s unindented filename header as an import path. It now separates that
header before whitespace trimming; actual imported paths, including indented
names starting with the header text, remain rejected. The new regression failed
before the change and passes afterward. All 122 xtask unit tests, three CLI
integration tests, and xtask Clippy passed.

## Local release gate

`cargo xtask rc-preflight` passed API, dependency, fuzz-build, feature, standard
validation and release-test stages. All nine AddressSanitizer fuzz campaigns
completed: **10,787,750 executions over 2,712 seconds**, with no reported
failure. Each retained the canonical 300-second time budget, 10-second input
timeout, and 2,048 MiB RSS limit. Real ARGOS/Huron integration tests also passed.

The next command failed:

```sh
cargo test --locked --test openslide_parity --features parity-openslide \
  preflight -- --exact --ignored
```

The checked-in public manifest is missing release evidence for:

- Hamamatsu VMU, Olympus VSI, Philips TIFF, slide cache, generic TIFF, Trestle,
  and Zeiss ZVI.
- Lossless HTJ2K byte equivalence.
- Real progressive-JPEG DICOM with SOF2.
- Independent DICOM JP2K YBR_RCT and YBR_ICT cases.

This is a **manifest/evidence gap, not proof that all those files are absent**.
The saved OpenSlide corpus contains VSI archives, Philips/generic TIFF, Trestle,
and ZVI. Independent OpenJPH HTJ2K fixtures also exist in J2K test support.
Inspection of saved DICOM archive prefixes found YBR_ICT candidates and SOF2
candidates that still need full metadata/frame and reference validation. No VMU
or YBR_RCT representative was established by this inventory. Existing files
were not relabeled as reviewed release evidence merely to satisfy the gate.
The bounded prefix inspection is not a complete DICOM parser or PHI review.

Because preflight stopped here, it did not reach its local coverage,
performance-comparison, or package stages. The separate hosted coverage and
package checks passed as stated above. No alias filter or scope waiver was
used to turn full release preflight green.

## Fresh performance attempt

Environment: Apple M4 Pro, 12 physical/logical CPU cores, 48 GiB RAM, macOS arm64,
Rust/Cargo 1.96.0; J2K family 0.10.0 and JXR 0.1.1. The release CPU shim used
`route-telemetry`, a 256 MiB API cache, client worker counts 1/2/12, and the
harness's matching codec thread budgets. Filesystem caches were uncontrolled;
these are not cold-filesystem measurements. Local builds and fuzz campaigns
finished before capture started; release worker and shim builds finished before
the first timed worker.

The exact attempted matrix was:

```sh
WSI_RS_PARITY_CORPUS_CACHE="$WSI_RS_CORPUS_ROOT" \
WSI_RS_OPENSLIDE_LIBRARY="$OPENSLIDE_4_0_1_LIBRARY" \
WSI_RS_PERF_RESULTS_DIR=target/release-performance \
WSI_RS_PERF_REPEATS=5 \
WSI_RS_PERF_WORKERS=1,2,12 \
WSI_RS_PERF_CACHE_BYTES=268435456 \
cargo xtask perf-capture-pair release-0.7.0 \
  svs-001 svs-jp2k-001 ndpi-001 vms-001 leica-001 ventana-001 mirax-001
```

It failed on NDPI repetition zero with:

```text
resource limit exceeded for decoded tile/associated output:
requested 1952972800 bytes, limit 134217728 bytes
```

Serial workload isolation with the same release library, one worker,
`RAYON_NUM_THREADS=1` and the same cache budget found that `zoom_trace` fails.
The other ten harness workloads completed: opening, single tile, both pan
levels, level-2 viewport, warm revisit, cache pressure, thumbnail, large region,
and batch export. These isolation runs are diagnostics, not a matched
five-repeat performance result.

The error comes from `Slide::region_source_work` estimating source tile output
before `read_region_fastpath` dispatch. Correcting this requires bounded source
work planning across that boundary; increasing the decoded-output limit would
not establish a safe fix. The complete paired capture was not produced, so no
new aggregate before/after throughput, latency distribution, RSS ratio, or
OpenSlide superiority claim is made. Historical three-repeat captures were
rejected by `perf-compare` because the release gate requires five alternating
process repetitions.

Previously completed optimization measurements and pixel-equivalence evidence
remain in [panning](panning-performance-2026-09-04.md),
[concurrency](panning-concurrency-2026-09-04.md),
[MIRAX opening](mirax-opening-2026-09-04.md), and
[reader opening](reader-opening-performance-2026-09-05.md). This report does not
extend those results to the failing zoom workload.

Local command logs, scanner reports, starting-state snapshots, benchmark
configuration, workload isolation JSON and downloaded GPU reports are retained
under `target/release-readiness-io3tfpoq`.

## Other repository updates

All seven Frames repositories were inspected for stale documentation and
manifest references. Relative Markdown links were checked across 128 documents.
Focused updates were pushed to the existing PR branches without merging their
unrelated unfinished code:

| Repository | Update | PR |
| --- | --- | --- |
| dicom-viewer | Zarr supply-chain version corrected to match the lockfile | [#2](https://github.com/frames-sg/dicom-viewer/pull/2) |
| wsi-dicom | Semver help and dependency-topology wording updated | [#12](https://github.com/frames-sg/wsi-dicom/pull/12) |
| wsi-dicom-annotations | Development and published API versions distinguished | [#1](https://github.com/frames-sg/wsi-dicom-annotations/pull/1) |
| wsi-annotation-interop | Development and published API versions distinguished | [#1](https://github.com/frames-sg/wsi-annotation-interop/pull/1) |
| JXR | Portable corpus extraction, regression tests, and verified CUDA results | [#1](https://github.com/frames-sg/jxr/pull/1) |
| J2K | No factual correction justified; no artificial change pushed | — |

JXR's [CUDA hardware run](https://github.com/frames-sg/jxr/actions/runs/33953370733)
passed after fixing the missing-`unzip` prerequisite. It reports 517 passing
in-scope T.834/T.835 cases, 179 established exclusions, lifecycle/ROI tests, and
20 checksum-checked benchmark cells. Its dated report records the one-device,
two-small-fixture limits. JXR's ordinary Rust/fuzz CI passed; the separate Metal
job remains queued for a Metal runner. CUDA availability does not satisfy a
Metal runner label.
