---
name: update-rust-toolchain
description: Update Loadout's supported Rust version and the CI and release toolchains. Use when raising Rust versions, checking the next stable version, or reviewing Rust toolchain alignment.
---

# Update Rust Toolchain

Keep the declared minimum supported Rust version and the pinned build toolchains aligned when the requested update includes supported-version changes. Do not change the Loadout package version or release metadata unless separately requested.

1. Check the [official Rust release list](https://blog.rust-lang.org/releases/) and the chosen version's announcement to confirm its exact stable patch version and review compatibility notes. Do not infer the current stable version from an installed toolchain or an old skill invocation. Review platform support for the CI hosts and release targets; native CI must confirm their builds.
2. Inspect `Cargo.toml`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`, and the worktree. State whether the update raises the `rust-version` support floor. Keep every CI and Release pin consistent with the selected stable version.
3. If raising the support floor, update `[package].rust-version` and describe the source-build compatibility change in the handoff. Preserve older-version support only when the user requests it and it can be verified.
4. Run the checks in `CONTRIBUTING.md` with the selected toolchain, including locked tests and a release build. Verify the declared minimum with that exact toolchain. Report any native platform checks that require CI, and inspect the complete diff for unrelated changes.

Follow `CONTRIBUTING.md` for Git and release operations. A toolchain update does not authorize a branch, tag, push, or release.
