# Internal Architecture

The public `Slide` and `SlideReader` surfaces stay format-independent. Internal
work is divided at validation, planning, I/O, and decode boundaries so that
format-specific state does not leak into the shared core.

## Region reads

`core::registry::composition` owns region behavior:

- `RegionReadPlan` validates scene, series, level, plane, geometry, and limits,
  then records tile hits and the selected integral or fractional mode.
- `RegionTileResolver` owns cache lookup, batched misses, result cardinality,
  cache insertion, concurrent-miss coalescing, and cache diagnostics.
- `integral` owns the exact single-tile return, typed clipped blits, and dense
  integral U8 row copies.
- `fractional_u8` owns interpolation and alpha accumulation. It allocates alpha
  storage only for fractional U8 work.
- `output` owns template selection, compatible output allocation, empty-region
  results, and RGB cropping.

`Slide` creates one validated plan and reuses its hits for source admission,
resolution and composition. Generic encoded-tile estimates are deferred until a
format region fast path declines the request; admission still validates its own
encoded bound and decoded staging first. A complete batch fitting the pre-composition
reservation retains dense composition even with one CPU worker. Streamed
consecutive batches fit decoded tiles plus codec work within the remaining staging
allowance, with separate encoded bounds and the CPU worker limit. Streaming targets
half the output allowance, capped at 1 MiB, for decoded/codec staging (or one
larger source); wider windows regressed measured RSS. This target changes execution granularity, not public resource or
cache limits. Singleton windows use direct tile resolution without batch vectors.
A source without sufficiently precise bounds streams single tiles. Whole-batch
integral composition keeps the exact-tile return and dense row-copy path; streamed
batches preserve hit order. Built-in readers reject unavailable region fast paths
on the caller thread. Custom readers retain their existing worker context. Cached
ordinary tiled regions avoid an otherwise empty worker dispatch. NDPI restart
regions also compose fully cached strips on the caller. A non-mutating cache
presence hint selects one worker handoff for incomplete regions; ordinary
resolution still handles eviction and sends late misses to the same pool. The
hint neither pins tiles nor changes admission, recency or cache counters. The
worker limit is queried from that same pool before planning. Synthetic NDPI fast paths retain their previous
worker context.
Dense composition plans clipped row spans once. Fractional composition precomputes
sampling axes only when the table fits the unused RGB/gray portion of the existing
RGBA output reservation; RGBA and thin strips retain scalar sampling. The float
weights, Pixman rounding, fused-operation order and alpha accumulation are unchanged.

## Decode execution and JPEG 2000

Automatic and CPU-only `DecodeRuntime` handles share one process-wide CPU pool.
Rayon callers reuse their invoking pool. Codec preparation receives the current
worker count explicitly. DICOM can resolve a complete native batch from existing
decoded frames before a worker handoff. A partial miss retains ordinary source
execution. Completed device decisions also use this fast path; calibration
continues to bypass decoded caches when comparing the routes.

`ReadExecutionContext` carries the enclosing operation reservation and cancellation
control through the private managed-reader boundary. Optional calibration and
native Metal input copies may reserve only immediately available operation and
slide headroom. A stack-owned atomic flag is shared by every context in one public
read, including separate admission chunks. A newly pending route consumes that
read's calibration opportunity; later internal batches cannot initialize the GPU
during the same first read. Subsequent reads advance at most one warmup/sample,
while selected routes may execute every admitted batch. Optional work cannot wait
while holding the ordinary reservation or bypass a FIFO admission waiter. Public
reader APIs and configured limits are unchanged.

`decode::jp2k::prepare` validates the unsigned RGB8 contract and logical dimensions.
Direct single-image CPU decodes consume the validated `J2kView`. Two-image batches
reuse the invoking CPU pool and consume each validated view once. Their two generic
native claims stay below the codec's existing four-claim ceiling. Singleton native
batches retain the original executor after the specialization regressed a constrained
concurrent workload. Larger borrowed CPU batches retain the codec's
aggregate allocation guards and parallel scheduler. Operation-local
`PreparedJp2kBatch` owners retain j2k 0.10.0 prepared groups for automatic route
comparisons, sharing encoded input and validated metadata between CPU and device
work. Metal consumes native prepared plans. CPU uses the established borrowed
batch executor: the j2k 0.10.0 owned CPU batch experiment changed lossy rounding
and regressed subsampled multithreaded batches, so it was rejected.
TIFF, DICOM and raw-JP2K preparation bypass decoded caches. No persistent metadata
cache is added. Metadata-only or unrepresentable codec plans retain their supported
strict single-image decode paths. Preparation keeps strict codec validation.

Automatic JP2K execution uses consecutive windows of at most 16 images and 4 MiB
of full-tile RGBA-equivalent output. This bounds simultaneously live comparison
outputs without reducing the request to a calibration sample. CPU-only reads and
strict device APIs retain their separate scheduling; individually larger images
retain existing admission. Automatic routing is foreground and bounded to 1,024
decisions:

1. A new eligible route returns CPU output and marks calibration pending without
   initializing the device.
2. A later eligible read warms the device once, outside route timing.
3. Three subsequent reads each measure one CPU/device pair from the same prepared
   inputs. Measurement order alternates; the entire execution window is measured.
4. The median device/CPU ratio must be at most 0.85 to select the device.

The key includes dataset, scene, series, level, codec, the full logical geometry
histogram and batch count, CPU worker count, and the initialized device identity
(Metal registry ID and name). Identity binding is lazy. The initial CPU read owns
the pending route until its output is ready. Later, one caller owns calibration;
competitors use CPU immediately. Busy entries cannot be evicted. Cancellation and
unwinding release ownership without publishing a partial measurement. Device failure
selects CPU for ordinary reads. Explicit resident APIs remain strict.

`jp2k::metal_batch` creates an operation-local `MetalBatchDecoder` over the retained
backend session, requests NHWC unsigned RGB8, submits compatible groups before
waiting within bounded execution windows, and restores original source slots.
Windows bound 16 images and target 4 MiB of physical RGBA-equivalent output to contain native
scratch retention; individually larger images retain their existing admission and
strict decoding rules. Prepared automatic batches regroup retained images without
reparsing. Color conversion still covers all eligible outputs in one pass.
Duplicates, mixed geometry and per-input errors remain ordered. Only batch capability rejection uses strict single Metal.
When extra encoded ownership cannot fit, admitted strict reads use single-image
Metal submissions. DICOM charges copies from actual frame sizes after loading
frames inside the ordinary reservation, allowing concurrent small native batches
without multiplying its conservative 128 MiB pre-index allowance. Optional
preparation forwards cancellation through source indexing and payload reads.
Logical crops precede one batch YCbCr conversion submission.
The converter uploads the immutable CPU lookup tables once and uses them in both
checked-u32 and u64-addressing shaders, giving exact CPU-compatible RGB conversion.

## DICOM

DICOM frame indexing is independent of tile decoding:

- `frame_index::model` owns immutable fragment references, frame ranges, and
  offset-table model data.
- `validation` owns fragment-graph and compressed-size limits.
- `offset_tables` reads and interprets Basic and Extended Offset Tables.
- `raw_little_endian` scans supported explicit-little-endian file layouts.
- `token_stream` provides the controlled parser fallback.
- `batch_io` turns an index into bounded grouped read spans, validates Item
  headers, and restores frame results by index.

`DicomFrameStore` owns the source path, native pixel location, lazy frame index,
and compressed-frame cache. `DicomImage` owns the decoded-frame cache alongside
its immutable image metadata.

`DicomBatchPlanner` validates requests and classifies each original result slot
as sparse black, cached, decodable frame, or device-ineligible. The CPU and
device reader modules consume the same plan metadata and restore output to the
original request order. `DicomReader` remains the thin `SlideReader` adapter.

All input-derived sizes use checked arithmetic and the shared resource limits.
Index publication is cancellation-aware, source replacement remains protected
by file identity checks, and `RequireDevice` never silently returns CPU data.

## Olympus VSI and device readback

The Olympus VSI module owns format probing and the reader adapter. `slide`
discovers companion ETS files and assembles ordered public scenes; `scene`
owns immutable metadata, with checked header and chunk-index parsing in its
`header` and `index` modules. `pixels` owns ETS payload reads, JPEG 2000
batch dispatch, and sparse background tiles. Each ETS scene retains its parsed
file handle and reads payloads positionally on Unix. Parsing retains the original
validation order and shared open budgets.

`output::download` materializes tightly packed CPU tiles from completed
device readback bytes. Metal and CUDA retain their own transfer, pitch, device
identity, and readback-limit checks. Completed immutable shared Metal storage
copies directly into the final CPU allocation, honoring offsets and padded rows.
Other storage uses a lazy retained session queue and a staging batch capped at the
existing 128 MiB download ceiling. Tight layouts encode one contiguous blit; padded
layouts retain row copies. Unsafe pointer/Metal interoperability stays confined to
`output::metal::interop`. Resident ownership survives crop, clone and session drop.

## CZI and JPEG XR

CZI preflight checks segment spans and configured metadata/index/input budgets
before the container library allocates or reads payloads. The WSI reader
accepts single-plane Bgr24 sources and rejects unsupported pixel/compression
contracts. `zeiss::composition` owns deterministic mosaic order and bounded
assembly; `subblock` owns codec adaptation; `raster` owns sample conversion and
clipped copies. Reconstruction runs outside the CZI seek lock.

`zeiss::source` owns subblock I/O and a byte-bounded LRU of decoded compressed
RGB blocks. Adjacent output tiles reuse these blocks; uncompressed sources keep
their clipped direct-copy path. CZI assigns half of its existing private-cache
budget to source blocks (16 MiB by default, enough for a 2056 × 2464 RGB block),
and divides the remainder between output tiles, whole levels, and associated
images. Oversized entries are decoded without retention. `zeiss::batch` resolves unique
source blocks once per admitted batch, bounds both actual decoded staging and
encoded/codec work, and composes outputs in deterministic order. It preserves
uncompressed clipped-row copies. Concurrent compressed-block misses share the
existing bounded flight machinery; local producers finish before callers wait,
and pool workers never wait for source flights. No cache or seek mutex is held
during decode.

Main-image JPEG/JPEG XR composition copies the codec's RGB rows directly into
the output. Typed embedded CZI attachments retain the container bitmap adapter.
Subblock preflight reuses one open file handle while retaining span, limit, and
source-identity checks. Cache hits validate source identity before reusing pixels.

`decode::jpegxr` is the shared CZI/TIFF adapter to the separate JXR crate.
It validates dimensions, precision, color and alpha, then applies bounded CPU
decode settings. TIFF owns physical-to-logical edge cropping.

JPEG 2000 metadata and coding support come from `j2k::J2kView`. The WSI layer
checks its unsigned RGB8 output contract and output budget, and leaves packet,
quantization, coding-style, and tile-part policy to the codec. The legacy parser
is compiled only in tests as an independent fixture oracle.

`core::decode_runtime::reader` owns forwarding and route geometry;
`adaptive` owns admitted execution, and `calibration` owns state transitions.
The parent owns reusable runtime/pool state and routing configuration.

## NDPI offset reuse

Generated NDPI levels use virtual tiles no larger than 256 by 256 pixels. Native tile reads use the existing cropped synthetic-level path; this keeps generic and fractional region planning from treating a whole generated level as one decoded tile. Physical level geometry and image coordinates remain unchanged.

NDPI borrows relative MCU offsets from TIFF's already validated immutable tag
allocation. The existing byte-bounded MCU cache retains a 128-byte classification
entry instead of a second copy of the offset array. High-word combination and
file-absolute normalization retain the existing separately owned, byte-weighted
array path. Cache keys still include the IFD, tag, strip offset, and strip length.
Disabled and undersized caches borrow relative offsets without retaining the
classification; they repeat the unchanged classification scan. No cache budget,
source identity check, payload validation, decode algorithm, or thread policy is
changed.

## Bounded region concurrency

NDPI integral region reads batch small restart strips inside the existing region
staging reservation. The batch is limited by output-to-strip geometry, existing
NDPI batch caps, and the current Rayon pool. Large strips retain one-at-a-time
streaming. NDPI region batches run in the existing pool and collect results in
request order before selecting the first error. Codec algorithms remain external.

`core::cache::flights` coordinates active shared region-cache misses by the full
tile key. It permits at most 128 producer records (fewer for small caches), keeps
only weak references in the registry, and retains no additional decoded tile
cache. Active callers may share the same decoded `Arc` even if the LRU evicts it.
The existing pixel-cache capacities and admission limits are unchanged; bounded
coordination bookkeeping is additional to pixel-payload accounting.

Region batches finish and publish their owned loads before waiting for other
batches, avoiding cycles between overlapping requests. Errors and unwinding
release ownership; callers retry failed shared work through their own source
path to preserve typed errors. Disabled/tiny caches bypass coordination. Rayon
workers and reentrant owners also bypass waiting, preventing pool starvation.
Explicit controlled tile APIs retain their existing cancellation boundaries.

## MIRAX and positional source access

`mirax::batch` deduplicates backing images within each admitted batch, resolves
bounded source groups in the existing pool, and restores logical crops in request
order. Actual decoded source sizes have a separate staging bound; an encoded
allowance cannot justify additional live decoded images. Concurrent source misses
reuse the existing flight coordinator and private-cache budget.

`core::positioned_file` owns retained handles for MIRAX records, VSI payloads and
`.svcache` payloads. Unix reads use explicit offsets. Other platforms serialize the
complete seek/read operation on one retained handle; cloned descriptors are never
assumed to have independent cursors. Source identity checks, `.svcache` format and
payload checksums retain their existing contracts.

## Performance diagnostics

Optional `route-telemetry` records adapter preparations, CPU batch sizes, actual
Metal group submissions, completion calls, color conversion and readback/staging
bytes, strict single-image decode calls and observed cached buffer-pool peaks.
These pool peaks do not measure all live GPU memory. Native benchmarks separate reference validation from timed reads and label
fresh readers versus warm revisits; a fresh reader is not cold disk or a cold
process. Perf-runner retains legacy wall-clock throughput and checksum semantics.
Its optional reader timing reports reader-active rates separately from verification
and bookkeeping, using the longest worker's accumulated reader-call time.
