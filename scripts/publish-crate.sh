#!/usr/bin/env bash
set -euo pipefail

dry_run="${DRY_RUN_ONLY:-false}"
crate="ziggurat"
version="$(cargo pkgid | sed 's/.*#//')"

if [[ "$dry_run" == "true" ]]; then
  cargo publish --dry-run
  exit 0
fi

: "${CRATES_IO_API_TOKEN:?CRATES_IO_API_TOKEN is required for a real publish}"

if cargo info "${crate}@${version}" --registry crates-io >/dev/null 2>&1; then
  echo "${crate} ${version} is already published; skipping"
  exit 0
fi

export CARGO_REGISTRY_TOKEN="$CRATES_IO_API_TOKEN"
cargo publish
