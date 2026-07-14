#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: scripts/publish-crate.sh --verify | --dry-run | --publish

  --verify   validate manifest and changelog release identity only
  --dry-run  validate release identity and run cargo publish --dry-run --locked
  --publish  require an exact GitHub release tag and OIDC token, then publish
USAGE
}

fail() {
  printf 'release validation failed: %s\n' "$*" >&2
  exit 1
}

release_version() {
  python3 - <<'PY'
from pathlib import Path
import tomllib

with Path("Cargo.toml").open("rb") as handle:
    manifest = tomllib.load(handle)
version = manifest.get("package", {}).get("version")
if not isinstance(version, str) or not version:
    raise SystemExit("Cargo.toml has no package.version")
print(version)
PY
}

validate_release_metadata() {
  local version="$1"
  python3 - "$version" <<'PY'
from pathlib import Path
import re
import sys

version = sys.argv[1]
changelog = Path("CHANGELOG.md").read_text(encoding="utf-8")
pattern = re.compile(
    rf"^## \[{re.escape(version)}\] - \d{{4}}-\d{{2}}-\d{{2}}$",
    re.MULTILINE,
)
matches = pattern.findall(changelog)
if len(matches) != 1:
    raise SystemExit(
        f"CHANGELOG.md must contain exactly one dated release heading for {version}"
    )
PY
}

validate_publish_ref() {
  local version="$1"
  local expected_tag="v${version}"
  local ref_type="${GITHUB_REF_TYPE:-}"
  local ref_name="${GITHUB_REF_NAME:-}"

  [[ "$ref_type" == "tag" ]] || fail "real publish requires GITHUB_REF_TYPE=tag"
  [[ "$ref_name" == "$expected_tag" ]] || \
    fail "tag ${ref_name:-<unset>} does not match manifest version ${expected_tag}"

  local head_sha tag_sha
  head_sha="$(git rev-parse HEAD)"
  tag_sha="$(git rev-list -n 1 "$ref_name")"
  [[ "$head_sha" == "$tag_sha" ]] || fail "tag $ref_name does not point at HEAD"
  if [[ -n "${GITHUB_SHA:-}" ]]; then
    [[ "$head_sha" == "$GITHUB_SHA" ]] || fail "GITHUB_SHA does not match HEAD"
  fi
  [[ -z "$(git status --porcelain --untracked-files=no)" ]] || \
    fail "tracked worktree is not clean"
}

main() {
  [[ "$#" -eq 1 ]] || {
    usage >&2
    exit 2
  }
  local mode="$1"
  case "$mode" in
    --verify | --dry-run | --publish) ;;
    -h | --help)
      usage
      return 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac

  local crate="wsi-rs"
  local version
  version="$(release_version)"
  validate_release_metadata "$version"

  if [[ "$mode" == "--verify" ]]; then
    printf 'release identity is valid for %s %s\n' "$crate" "$version"
    return 0
  fi

  if [[ "${GITHUB_REF_TYPE:-}" == "tag" ]]; then
    validate_publish_ref "$version"
  fi

  if [[ "$mode" == "--dry-run" ]]; then
    cargo publish --dry-run --locked
    return 0
  fi

  validate_publish_ref "$version"
  : "${CARGO_REGISTRY_TOKEN:?OIDC-provided CARGO_REGISTRY_TOKEN is required}"

  if cargo info "${crate}@${version}" --registry crates-io >/dev/null 2>&1; then
    fail "${crate} ${version} is already published"
  fi
  cargo publish --locked
}

main "$@"
