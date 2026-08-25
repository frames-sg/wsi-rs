# Supply-chain policy and temporary exceptions

Release dependency checks are reproducible through `cargo xtask deps`. The
command runs locked [Cargo Deny](https://embarkstudios.github.io/cargo-deny/),
Cargo Machete, and [Cargo Vet](https://mozilla.github.io/cargo-vet/). CI
installs exact reviewed tool versions.

`supply-chain/` is the Cargo Vet store. Imported audits come from the Bytecode
Alliance, Google, and Mozilla. Uncovered crates are explicit
`safe-to-deploy` exemptions, not implicit trust. Dependency updates must rerun
`cargo vet --locked`; new exemptions require review in the same change.

The J2K 0.10.0 family resolves from its coordinated crates.io release.
Exact-version Cargo Vet exemptions record local acceptance of its `objc2`,
SIMD, and split CUDA engine dependency surface; any later J2K version requires
another explicit review.

## Time-bound upstream exceptions

| Dependency | Surface and control | Owner | Review or expiry |
| --- | --- | --- | --- |
| `encoding 0.2.33` | Unmaintained transitive dependency of `dicom-encoding 0.9.1`. DICOM text parsing remains bounded by the format parsers and all known RustSec vulnerabilities are denied. | wsi-rs maintainers | 2026-10-01 or the next dicom-rs release, whichever is first |
