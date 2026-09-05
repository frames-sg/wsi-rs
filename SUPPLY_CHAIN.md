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

## Apple Silicon quickhash acceleration

On macOS/aarch64 only, `sha2 0.10.9` enables its `asm` feature to select the
crate's runtime-detected ARM SHA-256 backend. Its software fallback remains
available; other targets keep their previous features. The feature introduces
the MIT-licensed `sha2-asm 0.6.4` build dependency surface, using the already
locked `cc` crate and the platform assembler. No custom hash code or unsafe
code was added to wsi-rs. The exact package's manifest, build script, facade,
selected assembly and reachable integration were reviewed; the local Cargo Vet
audit records the scope. Known-answer tests cover hardware and forced-software
builds. No exemption was added. This requires the existing macOS C toolchain
at build time and adds no persistent runtime cache.

## Time-bound upstream exceptions

| Dependency | Surface and control | Owner | Review or expiry |
| --- | --- | --- | --- |
| `encoding 0.2.33` | Unmaintained transitive dependency of `dicom-encoding 0.9.1`. DICOM text parsing remains bounded by the format parsers and all known RustSec vulnerabilities are denied. | wsi-rs maintainers | 2026-10-01 or the next dicom-rs release, whichever is first |

## JPEG XR release gate

The reader now resolves `jxr 0.1.1`, `jxr-core 0.1.0`, `jxr-native 0.1.0`, and
`jxr-math 0.1.0` from crates.io. The source repository is
https://github.com/frames-sg/jxr. Exact-version `safe-to-deploy` reviews for all four CPU
packages are recorded in `supply-chain/audits.toml`; no exemptions were added
for this integration. `cargo xtask deps` verifies the advisory, license, source,
unused-dependency and audit gates. Later versions require a fresh review.
The CPU dependency audits do not establish optional JXR GPU conformance or
replace wsi-rs's own release validation.
