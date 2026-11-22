# Security Policy

## Reporting a Vulnerability

wsi-rs parses untrusted slide files and passes their tile data to codecs. Report crashes,
memory-safety bugs, wrong output, metadata leaks and unexpected file-system access privately, not in
a public issue.

Use GitHub's
[private vulnerability reporting](https://github.com/frames-sg/wsi-rs/security/advisories/new). If
it is unavailable, contact the maintainer through the repository owner's profile.

Include:

- The smallest input file or generated fixture that reproduces the problem.
- Rust version, target triple, operating system and Cargo features.
- The API call, command or OpenSlide-shim function that triggers it.
- Expected and observed behavior.

Reports are acknowledged within 7 days. Fixes ship as soon as possible, generally within 30 days for
high-severity issues.

## Supported Versions

The latest release on crates.io and `main`. Metal reports are triaged when supported macOS hardware
is available.
