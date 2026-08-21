# Internal Architecture

The public `Slide` and `SlideReader` surfaces stay format-independent. Internal
work is divided at validation, planning, I/O, and decode boundaries so that
format-specific state does not leak into the shared core.

## Region reads

`core::registry::composition` owns region behavior:

- `RegionReadPlan` validates scene, series, level, plane, geometry, and limits,
  then records tile hits and the selected integral or fractional mode.
- `RegionTileResolver` owns cache lookup, batched misses, result cardinality,
  cache insertion, and cache diagnostics.
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
