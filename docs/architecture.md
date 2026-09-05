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

The top-level region functions only coordinate these components.

## Decode execution and JPEG 2000

Decode runtime selection is carried by the crate-private
`ReadExecutionContext` at read-operation boundaries. JPEG 2000 workers do not
read thread-local or process-global runtime state.

`decode::jp2k::prepare` validates each request once and produces a concrete
`PreparedJp2kJob`. CPU, Metal, and CUDA paths consume that prepared job while
retaining backend-specific session and fallback policy. `decode::jp2k::output`
owns checked output materialization and cropping.

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
dispatch, and sparse background tiles. Parsing retains the original validation
order and shared open budgets.

`output::download` materializes tightly packed CPU tiles from completed
device readback bytes. Metal and CUDA retain their own transfer, pitch, device
identity, and readback-limit checks.

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
images. Oversized entries are decoded without retention. Concurrent cache
misses can decode independently; no cache or seek mutex is held during decode.

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

`core::decode_runtime::reader` owns adaptive read execution and forwarding;
the parent owns reusable runtime/pool state and routing configuration.

## NDPI offset reuse

NDPI borrows relative MCU offsets from TIFF's already validated immutable tag
allocation. The existing byte-bounded MCU cache retains a 128-byte classification
entry instead of a second copy of the offset array. High-word combination and
file-absolute normalization retain the existing separately owned, byte-weighted
array path. Cache keys still include the IFD, tag, strip offset, and strip length.
Disabled and undersized caches borrow relative offsets without retaining the
classification; they repeat the unchanged classification scan. No cache budget,
source identity check, payload validation, decode algorithm, or thread policy is
changed. See `docs/audits/panning-performance-2026-09-04.md` for measurements.


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

Measurements and limitations are in `docs/audits/panning-concurrency-2026-09-04.md`.
