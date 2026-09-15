# Loadout Release Validation

Run validation from the repository root using the toolchain required by the current repository instructions.

## Metadata check

Locate the loaded `SKILL.md` directory and invoke its adjacent checker with the intended tag:

```sh
bash "<skill-directory>/scripts/check-release-metadata.sh" vX.Y.Z
```

The checker verifies the package version, changelog heading, locked dependency resolution, and tag syntax. It is read-only apart from Cargo's normal local cache and metadata.

After the release pull request has merged, add `--require-main`:

```sh
bash "<skill-directory>/scripts/check-release-metadata.sh" vX.Y.Z --require-main
```

This requires `HEAD` to be reachable from the locally available `origin/main`, matching the tag workflow's provenance rule. If `origin/main` is unavailable, report that limitation; do not fetch or change refs merely to make the check pass unless authorized.

## Candidate checks

Run applicable checks and report each result:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked --bin loadout
git diff --check
```

Inspect the complete diff. It should contain only release metadata and justified release-supporting documentation.

An authorized `workflow_dispatch` run with its `version` input is an artifact-only candidate build. It does not create a GitHub Release.

## After an authorized tag push

A tag push is not proof that the release completed. Inspect the Release workflow for tag validation, all five target builds, checksum verification, and GitHub Release creation. Report failed or skipped jobs precisely.
