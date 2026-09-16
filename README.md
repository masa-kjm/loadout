# Loadout

Loadout is a local environment manager built around explicit desired state, ownership-aware filesystem operations, and durable recovery.

## Status

v0.2.0 is released. v0.3.0 development adds `init` while retaining the v0.2 lifecycle contracts.
The repository implements `init`, `validate`, `diff`, `plan`, and `apply` (including confirmation and dry run) as described in the [CLI specification](docs/specs/cli.md).
Platform support is incomplete: create, remove, relocate, replacement, and source-changing ownership handoff are enabled on Unix builds without filesystem-type filtering, with recorded native evidence on Linux/local ext4. The same action set is enabled through retained-parent primitives on Windows/local NTFS, with recorded native Windows evidence. macOS/APFS remains unverified. Other combinations are not claimed supported merely because a capability is enabled.
Release archives can be installed with the Unix and Windows installer scripts in `scripts/`.
The published package's Rust library target is not yet a supported public API.

v0.1 is retired and unsupported.
The published `loadout` v0.1.0 crate is preserved by the `v0.1.0` archive tag, and the final legacy source snapshot is preserved by `legacy/v0.1-final`.
v0.2.0 does not provide compatibility with v0.1 configuration, state, commands, resources, or behavior.

## v0.2.0 Direction

v0.2.0 begins with one complete, safe resource lifecycle: materializing a regular file from a local store as a file symbolic link below the current user's home directory.
It provides profile composition, validation, planning, drift inspection, conflict detection, state locking, verified application, and crash recovery.

The core planning contract is:

```text
Resolved Desired + Known + Actual -> Plan
```

Loadout checks recorded ownership and filesystem safety immediately before mutations, but does not exclude concurrent external changes. An entry substituted after the last check can be deleted or replaced, and successful postcondition verification may not reveal the race. See the [file-link concurrency contract](docs/specs/file-link.md#external-filesystem-concurrency) for the scope and limits of these guarantees.

The intended completion baseline is Linux/local ext4, macOS/local APFS and Windows/local NTFS for the complete file-link lifecycle. See [supported scope](docs/specs/file-link.md#intended-supported-scope) for exclusions and evidence gates; intended support is distinct from the current capability status above.

## Installation

After a v0.2.0 GitHub Release is published, install its latest archive with one of the following commands:

```sh
curl -fsSL https://raw.githubusercontent.com/masa-kjm/loadout/main/scripts/install.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/masa-kjm/loadout/main/scripts/install.ps1 | iex
```

Both installers accept an exact release tag through `--version vX.Y.Z` or `-Version vX.Y.Z` and install below `~/.local/bin` or `%USERPROFILE%\.local\bin` by default. They download and verify the release archive's SHA-256 sidecar before installation.

## Basic Usage

Loadout v0.2.0 uses an environment configuration and one or more profile files. The local store can live anywhere; it does not need to be inside the directory containing the configuration. For example:

```text
loadout-config/
├── config.yaml
└── profiles/
    └── workstation.yaml

~/src/dotfiles/
└── gitconfig
```

`config.yaml`:

```yaml
schema_version: 1
default_profile: workstation

profile_discovery:
  paths:
    - ./profiles

stores:
  dotfiles:
    type: local
    path: ~/src/dotfiles
```

`profiles/workstation.yaml`:

```yaml
schema_version: 1
id: workstation
resources:
  git-config:
    type: file
    properties:
      kind: file
      source:
        store: dotfiles
        path: gitconfig
      target: ~/.gitconfig
      operation: link
```

The source file must already exist, and the target's parent directories must already exist. Paths such as `./profiles` are relative to `config.yaml`, while `~/src/dotfiles` is resolved from the user's home directory. From `loadout-config/`, validate and preview the changes:

```sh
loadout validate --config ./config.yaml
loadout plan --config ./config.yaml
```

Apply the plan after reviewing it:

```sh
loadout apply --config ./config.yaml
```

Use `--yes` when running `apply` non-interactively. `--dry-run` performs the apply lifecycle without changing the filesystem. To inspect managed state and drift, run:

```sh
loadout diff
```

### Initialize a portable environment

To start an empty environment repository, run this command from its root:

```sh
loadout init
loadout validate --config ./.loadout/config.yaml
```

`init` creates only `.loadout/config.yaml` and `.loadout/profiles/base.yaml`; it does not change runtime configuration, initialize Git, or modify native assets.

## Documentation

The authoritative v0.3 documentation is in [`docs/`](docs/README.md).

- [Architecture](docs/architecture/README.md) defines system responsibilities and boundaries.
- [Specifications](docs/specs/README.md) define the v0.3.0 observable contracts.
- [Testing Strategy](docs/development/testing.md) defines the required evidence for those contracts.
- [Future Considerations](docs/future/README.md) records non-binding work outside v0.3.0.

`docs/architecture/` and `docs/specs/` are authoritative for v0.3.0.
Future and draft material may inform a later design, but it cannot change a published v0.3.0 contract.
