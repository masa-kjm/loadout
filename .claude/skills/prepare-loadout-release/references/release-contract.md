# Loadout Release Contract

[`CONTRIBUTING.md`](../../../../CONTRIBUTING.md) and [`.github/workflows/release.yml`](../../../../.github/workflows/release.yml) are authoritative. This reference is a release-review checklist and does not replace them.

## Release candidate contents

For target tag `vX.Y.Z`:

- The root `[package].version` in `Cargo.toml` is `X.Y.Z`.
- `CHANGELOG.md` contains the exact heading `## vX.Y.Z` before the tag is pushed.
- `Cargo.lock` resolves with `--locked`.
- Release-supporting documentation reflects actual installation and compatibility guidance without rewriting historical specifications or changelog entries.
- A release-only pull request contains release metadata and necessary documentation, not unrelated product behavior changes.

The target tag must be an exact `vX.Y.Z` spelling of the package version and point to a commit reachable from `main`.

## Distribution boundary

The v0.2.0 workflow publishes archive assets only. It builds the `loadout` binary and includes `README.md`, `LICENSE`, and `CHANGELOG.md` for these targets:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

Every archive has a SHA-256 sidecar. The workflow does not publish to crates.io, use a registry token, or update an external package tap.

The repository token is read-only by default. Only the final GitHub Release job receives `contents: write` after validation, all builds, and checksum verification succeed.

## Incident boundary

Treat a failed or partial release as an incident. Do not move or recreate the tag, overwrite assets, or blindly rerun the workflow. Inspect the recorded workflow and release state, correct the cause, and use a new patch version when a new immutable release is required.
