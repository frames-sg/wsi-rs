# WSI reader architecture audit — 2026-09-04

Scope: `wsi-rs` 0.7.0 checkout, its production reader/decode boundaries,
CZI promotion from test-only code, and the published JXR dependency. The target
is a WSI reader: source discovery, bounded metadata/index parsing, tile/frame
addressing, source pixel interpretation, and requested-region composition.
Codec algorithms belong to J2K/JXR. Existing cache-file APIs remain compatible;
no new slide writing, transcoding, annotation, inference, or viewer UI is added.

## Findings and applied changes

| Priority | Finding | Resolution |
| --- | --- | --- |
| P1 | TIFF LZW/Deflate/Zstd accepted short decoded payloads because the destination was prefilled with zero and only excess output was rejected. | Require the exact decoded byte count before predictor reversal. The regression covers all three codecs. |
| P1 | The legacy CZI raw-pixel path silently padded short payloads and truncated excess bytes. | Require exact lengths. Invalid source pixels produce an error. |
| P1 | The CZI default-plane predicate always returned true; distinct Z/C/T planes could overlap in one image. | Filter against the observed first coordinate, and reject multi-plane WSI datasets at open instead of collapsing axes. |
| P1 | Missing/unsupported CZI reduced-level pixels could be replaced with a resized base image. | Return an explicit source-composition error. Native-level reads preserve source semantics. |
| P1 | Overlapping CZI subblocks used different ordering in tile and whole-level reads. | One compositor orders by mosaic index and file position for both paths. |
| P1 | The reader had a second JPEG 2000 parser and a coding-policy gate which rejected a valid multi-tile/multi-part stream supported by J2K. | Production inspection now delegates to `j2k::J2kView`; WSI retains unsigned RGB8/output-size requirements. Coding styles, quantization, and packet/tile-part support belong to the codec. |
| P2 | CZI parsed with fixed constants despite caller-supplied metadata, index, and encoded-unit limits. | Apply configured limits before parser allocations and source reads, and account for the expanded canvas index. |
| P2 | CZI's roughly 950-line pixel module mixed codec adaptation, raster copies, tile addressing, and whole-level composition; the two composition implementations had drifted. | Separate these responsibilities and share composition. Decode no longer holds the seek lock across reconstruction. |
| P2 | The roughly 1,000-line decode-runtime module mixed runtime/pool state with reader forwarding and route execution. | Separate the adaptive reader implementation from runtime state while preserving all routing/cancellation tests. |

The old JPEG 2000 implementation is retained only under `cfg(test)` as an
independent fixture metadata oracle, together with every existing parser test.
It is not compiled into the reader. New differential tests check geometry,
sampling, precision, sign, and MCT metadata against that oracle; a generated
multi-tile/multi-part stream exercises the newly delegated codec boundary.

## Codec and format boundaries

A single `decode::jpegxr` adapter validates expected dimensions, sample type,
color/alpha contract, and resource ceilings, then calls the published JXR CPU
decoder. CZI uses compression code 4. Tiled TIFF uses code 22610 and decodes the
physical tile before cropping its logical edge. There is no new codec engine
or GPU reconstruction implementation in WSI.

The initial public CZI WSI slice is single-plane Bgr24 with uncompressed,
JPEG, or JPEG XR subblocks. Source scenes are represented on the existing
canvas; only common pyramid resolutions are exposed. JPEG XR Bgr48 preview
attachments preserve U16 samples. Multi-plane CZI, unsupported pixel types,
and unsupported attachment codecs remain explicit errors. TIFF JPEG XR support
is limited to contiguous unsigned 8-bit grayscale/RGB tiles, top-left
orientation, no predictor, and no alpha. Generic compressed strips are not
newly claimed.

## Review decisions

Large files alone were not treated as defects. TIFF vendor interpreters and
DICOM indexing have format-specific responsibilities and existing tests; they
were not split just to meet a line count. Repeated trait forwarding methods
are adapter obligations, not duplicated codec algorithms. The cache builder
and public display/region methods predate this change; removing them would
break callers and is outside this compatibility-preserving reader audit.

The JPEG XR source is maintained at https://github.com/frames-sg/jxr. The
published CPU dependency is separate from optional Metal/CUDA/tensor adapters.
The local JXR T.834/T.835 CPU run passed 517 in-scope cases and excluded 179
out-of-scope cases, with no failures or unsupported harness cases. This does
not establish universal conformance or CUDA hardware correctness.

## Corpus evidence and limits

The public OpenSlide Zeiss corpus identifies `Zeiss-5-JXR.czi` and
`Zeiss-5-Cropped.czi` as CC0 mouse-kidney brightfield samples supplied by
Venklab, Pathology, UTHSCSA. Both were exercised for native-level tile reads,
ordered/repeated batches, regions, and associated images. Sampling follows
actual encoded subblocks because the canvas center falls between tissue scenes.
See https://openslide.cs.cmu.edu/download/openslide-testdata/Zeiss/.

Synthetic fixtures use an original RGB gradient encoded losslessly by the
external T.835 reference executable. They verify exact CZI/TIFF RGB pixels and
TIFF edge cropping without distributing reference software or ITU vectors.

An audit and passing tests cannot prove that every WSI file is supported or
that all defects are absent. The supported subset above and the final
validation results are the operative claims.

## Release blockers

`cargo xtask deps` passes advisory/license/source and unused-dependency checks,
then fails because the four new CPU JXR crates lack `safe-to-deploy` Cargo Vet
attestations. No new exemptions were added. This is a reader release blocker.

The JXR repository's hosted macOS CI cannot execute the low-level Metal tests:
its virtual device lacks the required M1-or-newer Apple GPU capabilities.
Those tests pass on the local Apple GPU, and Linux all-target Clippy passes.
A supported hardware CI runner is still required for the complete GPU gate;
CUDA hardware validation is unclaimed.

## Final local validation

| Command/check | Result |
| --- | --- |
| `cargo xtask validate` | Formatting, Clippy, benchmark/shim release builds, default/OpenSlide/Metal-parity tests, doctests and docs passed. The largest feature suite ran 1,010 passing tests; corpus/hardware tests marked ignored were not counted as exercised. |
| `cargo xtask feature-check` | All 39 configured checks passed. |
| `cargo xtask coverage` | Passed all existing floors: 84.95% workspace line coverage and 80.14% function coverage. |
| `cargo xtask coverage-changed --base HEAD --lcov lcov.info --threshold 80` | Passed: 92.39% changed-line coverage. |
| `WSI_RS_UPDATE_PUBLIC_API=1 cargo xtask api-check` | Generated/reviewed default, Metal and CUDA snapshots; each adds only the non-exhaustive `Compression::JpegXr` variant. The existing semver tool treats its 0.6-to-0.7 baseline as a major transition and skips compatibility lints. |
| `cargo xtask fuzz-check` | All nine targets built with a stable reviewed lockfile. The generic WSI target now has a valid CZI/JPEG XR seed; its pixel-read integration test passes. No new mutation campaign is claimed. |
| `cargo package --locked --allow-dirty` | Package verified against registry dependencies. `--allow-dirty` preserves the requested uncommitted WSI changes. |
| `cargo test --test zeiss_czi real_jpegxr -- --ignored` | Passed separately with `WSI_RS_CZI_JXR_PATH` set to the downloaded standard and cropped JPEG XR CZI samples. |
| `cargo xtask deps` | Advisory, license, source and unused-dependency checks passed; Cargo Vet failed for the four new CPU JXR crates, as recorded above. |

The new CZI seed generator was exercised only for its valid CZI output, and
the generated fuzz payload was checked against the packaged regression fixture.
The final diff and Git status were reviewed. WSI changes remain local and
uncommitted; the separately authorized JXR repository and crates are published.
