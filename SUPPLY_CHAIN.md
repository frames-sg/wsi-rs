# Supply-chain policy and temporary exceptions

Release dependency checks are reproducible through `cargo xtask deps`. The
command runs locked Cargo Deny, Machete, Cargo Audit for both root and fuzz
lockfiles, and Cargo Vet. CI installs exact reviewed tool versions.

`supply-chain/` is the Cargo Vet store. Imported audits come from the Bytecode
Alliance, Google, and Mozilla. Uncovered crates are explicit
`safe-to-deploy` exemptions, not implicit trust. Dependency updates must rerun
`cargo vet --locked`; new exemptions require review in the same change.

## Time-bound upstream exceptions

| Dependency | Surface and control | Owner | Review or expiry |
| --- | --- | --- | --- |
| `encoding 0.2.33` | Unmaintained transitive dependency of `dicom-encoding 0.9.1`. DICOM text parsing remains bounded by the format parsers and all known RustSec vulnerabilities are denied. | wsi-rs maintainers | 2026-10-01 or the next dicom-rs release, whichever is first |
| `paste 1.0.15` | Unmaintained build-time macro dependency in the optional Metal stack. It processes repository-controlled tokens, not slide input. | wsi-rs maintainers | 2026-10-01 or the next Metal-stack release, whichever is first |
| `block 0.1.6` | `metal 0.33` is deprecated and triggers Rust's `uninhabited_static` future-incompatibility warning. The optional Metal stack remains tested on pinned Rust 1.96, but must migrate through `j2k` to `objc2-metal`; suppressing or locally forking the warning is not accepted as resolution. | wsi-rs and j2k maintainers | Before raising MSRV beyond 1.96, before the lint becomes a hard error, or 2026-10-01, whichever is first |

The `block` warning is a known upstream release risk. It does not affect the
default feature set, CUDA, or memory-safe Rust code in `wsi-rs`, but a future
compiler may make the optional Metal build fail. A 0.5 release must state the
pinned compiler requirement for Metal until the upstream migration lands.
