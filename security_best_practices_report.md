# wsi-rs security audit

**Audit date:** 2026-07-31

**Scope:** `wsi-rs`, `wsi-rs-openslide-shim`, their parser and decode boundaries, dependency gates, and compatibility checks against the adjacent `dicom-viewer` workspace.

**Context:** Research-only use with no patient data. Privacy, de-identification, HIPAA, authentication, authorization, deployment isolation, and operational monitoring were therefore not assessed.

## Executive summary

The audit found two high-impact security classes: attacker-controlled resource consumption across several image/container parsers and unsafe path handling in the OpenSlide shim installer. The current worktree adds explicit input, collection, nesting, frame, and decoded-output budgets; moves DICOM preflight and parsing onto the same open file; checks the DICOM source before native frame reads; and makes installer staging and manifest creation exclusive while rejecting symlink and forged-manifest paths.

No confirmed memory-safety defect, exposed secret, or direct code-execution path was found. This was a repository-level review and verification pass, not an independent penetration test, and the fuzz targets were compiled but not run as time-bounded campaigns.

The hardened tree is suitable for controlled research workflows with no patient data. It is **not ready for a production release** until the open release blockers are resolved, particularly the unreviewed j2k 0.8 dependency set, patch-release API drift, removed release-policy tests, and missing pinned nightly toolchain.

## Findings

### WSI-001 — Unbounded parser and image resource consumption

**Severity:** High

**Status:** Remediated in the current worktree

Malformed or unusually large slide files could previously drive large allocations, collection growth, deep nesting, or whole-payload reads before the application rejected the input. The practical impact is process memory exhaustion, severe latency, or process termination when opening an untrusted or corrupt local slide.

The remediation introduces shared 512 MiB compressed-input and decoded-image ceilings (`src/core/limits.rs:4-40`) and format-specific limits before allocation or collection growth. Notable boundaries include:

- DICOM File Meta, element, cumulative value, token, and nesting limits, plus direct native-frame reads (`src/formats/dicom/preflight.rs:6-12`, `src/formats/dicom/preflight.rs:80-179`, `src/formats/dicom/image.rs:300-332`).
- CZI directory, subblock, metadata, attachment, dimension, and segment-range validation (`src/formats/zeiss/preflight.rs`).
- Olympus ETS dimension, scene, axis, tile-count, and payload-range validation (`src/formats/olympus_vsi.rs`).
- Hamamatsu VMS shard, index, header, and aggregate JPEG limits (`src/formats/hamamatsu_vms/jpeg.rs`).
- TIFF/Trestle decoded-output and dense irregular-tile-map limits (`src/formats/tiff_family/pixel_access/associated.rs`, `src/formats/tiff_family/layout/trestle.rs`).
- ZVI stream, plane, tag, axis, and decoded-canvas limits (`src/formats/zeiss_zvi.rs`, `src/formats/zeiss_zvi/compound.rs`, `src/formats/zeiss_zvi/slide.rs`).

Regression tests cover over-limit inputs, range overflow, duplicate keys, extreme coordinates, and bounded reads. The fixed per-operation limit does not provide a process-wide memory quota: a production service would still need bounded concurrency and admission control to prevent many valid 512 MiB operations from exhausting aggregate memory.

### WSI-002 — OpenSlide shim installer path following

**Severity:** High when an attacker can write the target prefix; otherwise Low

**Status:** Pre-existing symlink attacks remediated; residual race risk documented

Existence checks that followed symlinks, combined with non-exclusive temporary and staging writes, allowed a pre-created broken symlink to redirect creation to another path. In a privileged install into an attacker-writable prefix, this could overwrite or create files outside the intended library directory.

Manifest and staging files now use exclusive creation, path existence uses `symlink_metadata`, existing destination/stage/backup conflicts are rejected, restore entries are restricted to supported library names inside the canonical prefix, and destination and backup symlinks are rejected before mutation (`wsi-rs-openslide-shim/src/install.rs:333-365`, `wsi-rs-openslide-shim/src/install.rs:368-451`, `wsi-rs-openslide-shim/src/install.rs:538-610`). Regression coverage includes broken stage and manifest-temp symlinks and a forged backup symlink (`wsi-rs-openslide-shim/tests/install_contract.rs:226-319`).

There are still path-based check/use windows around rename operations. A complete defense against a concurrently malicious prefix owner would require descriptor-relative operations such as `openat`/`renameat` throughout. Production installation should require the prefix and its library directory to be owned by the installing administrator and not writable by less-privileged users.

### WSI-003 — DICOM source replacement after metadata parsing

**Severity:** Medium

**Status:** Remediated for normal file replacement

Separating metadata parsing from later native frame reads allowed a file at the same path to be replaced between operations, potentially applying trusted offsets and dimensions to a different file. DICOM preflight and metadata parsing now share one open descriptor (`src/formats/dicom/preflight.rs:27-51`), and later native reads compare a captured file identity before using cached offsets (`src/formats/dicom/image.rs:322-332`, `src/core/file_identity.rs:21-62`).

The identity is based on canonical path, length, modification time, and file kind, rather than a content digest. Filesystems or attackers able to preserve all of those attributes can evade the check; deployments that accept concurrently mutable hostile files should copy inputs into an owned immutable staging area before opening them.

### WSI-004 — Malformed JP2K header arithmetic and parser growth

**Severity:** Medium

**Status:** Remediated in the current worktree

Untrusted JP2K marker streams could drive unbounded marker/tile-part growth, extreme decomposition values, invalid shifts derived from code-block exponents, or oversized decoded output. The parser now caps markers and tile parts (`src/decode/jp2k_codestream.rs:14-16`, `src/decode/jp2k_codestream.rs:403-410`, `src/decode/jp2k_codestream.rs:470-479`), validates decomposition and exponent values before arithmetic (`src/decode/jp2k_codestream.rs:616-635`), and enforces the shared decoded-image budget.

The unused packet-tree parser is test-only, removing its attacker-controlled allocations from normal builds. Direct regression tests remain for its packet parsing behavior.

### WSI-005 — Upstream embedded-CZI decompression allocation

**Severity:** Medium

**Status:** Mitigated by rejecting the affected encoding

The upstream `czi-rs` path used for embedded CZI attachments can decode compressed subblocks through an unbounded convenience API before this crate regains control. The attachment path now preflights the embedded CZI, rejects compressed embedded subblocks, and validates the aggregate uncompressed plane canvas before calling upstream allocation code (`src/formats/zeiss/attachments.rs`).

This is a deliberate compatibility restriction: compressed embedded-CZI associated images are reported as unsupported. Re-enable them only after the dependency exposes a bounded streaming decode or an equivalent pre-allocation limit.

### WSI-006 — JPEG preparation and XML tree allocation budgets

**Severity:** Medium

**Status:** Remediated in the current worktree

JPEG table concatenation, dimension patching, and EOI repair could clone or concatenate inputs without checking their combined size. XML attributes could allocate key/value strings before enforcing a useful per-node boundary. JPEG now validates the combined prepared length before allocation (`src/decode/jpeg/input.rs:15-77`); XML enforces input, depth, node, global attribute, and pre-allocation per-node attribute budgets (`src/decode/xml.rs:6-49`, `src/decode/xml.rs:141-236`).

### WSI-007 — ZVI coordinate and collection overflow cases

**Severity:** Low

**Status:** Remediated in the current worktree

Extreme signed mosaic coordinates and attacker-controlled container counts could overflow arithmetic or grow collections excessively. The ZVI reader now bounds streams, planes, tags, and axes, validates decoded plane sizes, and uses non-overflowing coordinate-distance arithmetic. A regression covers the `i64::MIN`/`i64::MAX` mosaic case.

### WSI-008 — Unsafe and FFI review

**Severity:** Informational

**Status:** No confirmed defect found

Unsafe code remains confined to the intended Metal interoperability boundary and is guarded by the repository integrity check. The OpenSlide associated-image copy path validates source/destination lengths and clears the destination on failure. No confirmed out-of-bounds copy, invalid ownership transfer, or unsafe lifetime defect was identified during this pass.

## Remaining dependency and implementation risks

- The `cfb` dependency parses part of a ZVI compound-file directory before application-level stream-count limits run. No exploitable defect was confirmed, but this boundary remains dependency-owned.
- The shim installer still has path-based race windows as described in WSI-002.
- Per-operation memory limits do not cap aggregate memory across concurrent reads or decodes.
- Fuzz targets compiled successfully, but this audit did not execute sustained fuzzing or sanitizer campaigns.
- The explicit RustSec ignores used by the repository still require their existing risk acceptance; an audit command succeeding with an ignore is not equivalent to having no advisory.

## Production release blockers

| ID | Blocker | Required resolution |
| --- | --- | --- |
| RB-001 | Cargo Vet has no audits/exemptions for fourteen j2k 0.8 crates. | Complete the j2k review or add justified, version-specific audit records. Do not carry forward 0.7.2 exemptions without review. |
| RB-002 | The `0.5.2` worktree contains breaking public API changes and removes substantial API/docs/release-policy tests. | Restore compatibility for a patch release, or explicitly change the release/versioning plan and reinstate equivalent policy coverage. |
| RB-003 | The pinned `nightly-2026-04-17` toolchain is unavailable locally. | Install/provide the exact pinned toolchain and run the canonical API and fuzz checks reproducibly. |
| RB-004 | `block 0.1.6` emits a future-incompatibility warning. | Upgrade or replace it through the owning dependency path before a compiler update makes it a build failure. |

## Verification performed

| Command | Result |
| --- | --- |
| `cargo fmt --all` | Passed. |
| `cargo test -p wsi-rs --lib` | Passed: 634 tests. |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Passed; Cargo still reports the `block` future-incompatibility notice. |
| `cargo xtask validate` | Passed, including locked Clippy, benchmark compilation, default/parity Nextest suites, doctests, and docs. |
| `cargo xtask feature-check` | Passed: 18/18 feature combinations. |
| `cargo xtask deps` | Cargo Deny and Machete passed. Cargo Vet failed only for the fourteen unreviewed j2k 0.8 crates described in RB-001. |
| `cargo audit --file Cargo.lock --ignore RUSTSEC-2021-0153 --ignore RUSTSEC-2024-0436` | Passed with the two explicit ignores shown in the command. |
| `cargo audit --file fuzz/Cargo.lock --ignore RUSTSEC-2021-0153` | Passed with the explicit ignore shown in the command. |
| `cargo xtask fuzz-check` | Could not run because `nightly-2026-04-17` is missing. Each of the seven fuzz targets passed `cargo +nightly fuzz check` with the installed adjacent `rustc 1.97.0-nightly (2026-04-16)`. |
| `cargo xtask api-check` | Could not run because the pinned nightly is missing. `cargo +nightly public-api -p wsi-rs -sss --color never` ran successfully and confirmed that the checked-in snapshots currently differ. |
| Adjacent `dicom-viewer` format, Clippy, workspace tests, CUDA check, and Cargo Deny | Passed without modifying that repository: 243 tests passed and one fixture-dependent test was ignored. |

## Disposition

For the stated controlled, no-patient-data research use, the current hardened worktree is reasonable to continue testing. Treat slide files as untrusted input and avoid concurrent high-memory decodes until a process-wide resource policy exists.

Do not label or publish `0.5.2` as production-ready until RB-001 through RB-003 are closed and the remaining risks above have an explicit deployment decision.
