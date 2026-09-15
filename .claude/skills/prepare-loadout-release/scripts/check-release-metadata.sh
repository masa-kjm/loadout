#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: check-release-metadata.sh vX.Y.Z [--require-main]

Check the Loadout package version, CHANGELOG heading, locked Cargo resolution, and tag syntax.
--require-main also requires HEAD to be reachable from the locally available origin/main.
USAGE
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_main=0

if [[ "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -lt 1 || $# -gt 2 ]]; then
  usage >&2
  exit 2
fi

tag="$1"
if [[ $# -eq 2 ]]; then
  [[ "$2" == "--require-main" ]] || {
    usage >&2
    exit 2
  }
  require_main=1
fi

version="${tag#v}"
[[ "$tag" == "v${version}" && "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || fail "release tag must use the vX.Y.Z format"

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$script_dir" rev-parse --show-toplevel)" \
  || fail "run this script from a Loadout Git checkout"
cd "$repo_root"

manifest_version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)"
[[ -n "$manifest_version" ]] || fail "package version is missing from Cargo.toml"
[[ "$manifest_version" == "$version" ]] \
  || fail "Cargo.toml version is ${manifest_version}, expected ${version}"

grep --fixed-strings --line-regexp "## ${tag}" CHANGELOG.md > /dev/null \
  || fail "CHANGELOG.md must contain: ## ${tag}"

cargo metadata --locked --no-deps --format-version 1 > /dev/null \
  || fail "Cargo.lock is stale or Cargo metadata could not be resolved with --locked"

if [[ "$require_main" -eq 1 ]]; then
  git rev-parse --verify --quiet origin/main > /dev/null \
    || fail "origin/main is unavailable; fetch it before using --require-main"
  git merge-base --is-ancestor HEAD origin/main \
    || fail "HEAD is not reachable from the locally available origin/main"
fi

printf 'ok: %s matches Cargo.toml, CHANGELOG.md, and Cargo.lock\n' "$tag"
