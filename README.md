# Loadout

Loadout is a local environment manager built around explicit desired state, ownership-aware filesystem operations, and durable recovery.

## Status

v0.7.0 is the latest tagged release. The current source tree and [documentation](docs/README.md) define the unreleased v0.8.0 contract, including explicit schema-4-to-5 migration and resource ordering. The package manifest still reports `0.7.0`; it is not a v0.8.0 release marker.
The current command surface is defined by the [CLI specification](docs/specs/cli.md).
Release archives can be installed with the Unix and Windows installer scripts in `scripts/`; these install the latest tagged release by default.
The published package's Rust library target is not yet a supported public API.

Normal v0.8.0 commands reject earlier profile and state schemas; only the explicit [migration commands](docs/specs/migration.md) read complete valid schema-4 inputs.

## Current Source Capabilities

The current source tree materializes a regular file from a local store as either a file symbolic link or a content-owned regular-file copy, and a local-store real directory as a directory symbolic link.
It provides profile composition, validation, planning, drift inspection, conflict detection, state locking, verified application, crash recovery, and read-only inspection of declarations, Desired resources, Known state, and Actual observations.
Profile composition supports typed parameters. Schema-5 profiles add stable instance-scoped stages and direct resource prerequisites, which the planner schedules deterministically. The explicit `migrate state` and `migrate profiles` commands convert supported v0.7.0 schema-4 inputs in that order.

Loadout checks recorded ownership and filesystem safety immediately before mutations, but does not exclude concurrent external changes. An entry substituted after the last check can be deleted or replaced, and successful postcondition verification may not reveal the race. See the [file-link concurrency contract](docs/specs/file-link.md#external-filesystem-concurrency) for the scope and limits of these guarantees.

The documented platform baseline is Linux/local ext4, macOS/local APFS, and Windows/local NTFS. See [File Links](docs/specs/file-link.md#intended-supported-scope), [File Copies](docs/specs/file-copy.md#platform-requirements), and [Directory Links](docs/specs/directory-link.md#platform-requirements) for the exact scope and evidence requirements.

## Installation

After a GitHub Release is published, install its latest archive with one of the following commands:

```sh
curl -fsSL https://raw.githubusercontent.com/masa-kjm/loadout/main/scripts/install.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/masa-kjm/loadout/main/scripts/install.ps1 | iex
```

Both installers accept an exact release tag through `--version vX.Y.Z` or `-Version vX.Y.Z` and install below `~/.local/bin` or `%USERPROFILE%\.local\bin` by default. They download and verify the release archive's SHA-256 sidecar before installation.
The example below uses the current source-tree schema and requires a build from this tree until v0.8.0 is released; the latest tagged v0.7.0 archive uses schema-4 profiles.

## Basic Usage with the Current Source Tree

Loadout retains the environment configuration and profile-file model. The local store can live anywhere; it does not need to be inside the directory containing the configuration. For example:

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
schema_version: 2
default_profile: workstation

profile_discovery:
  paths:
    - ./profiles

stores:
  dotfiles:
    type: local
    properties:
      path: ~/src/dotfiles
```

`profiles/workstation.yaml`:

```yaml
schema_version: 5
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
      operation: copy
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

Read-only inspection commands report facts without planning or repairing a resource:

```sh
loadout status --config ./config.yaml
loadout profile list --config ./config.yaml
loadout resource list --config ./config.yaml
loadout resource list --known
```

### Initialize a portable environment

To start an empty environment repository, run this command from its root:

```sh
loadout init
loadout validate --config ./.loadout/config.yaml
```

`init` creates only `.loadout/config.yaml` and `.loadout/profiles/base.yaml`; it does not change runtime configuration, initialize Git, or modify native assets.

## Documentation

The authoritative current-source contract is in [`docs/`](docs/README.md).

- [Architecture](docs/architecture/README.md) defines system responsibilities and boundaries.
- [Specifications](docs/specs/README.md) define the observable contracts.
- [Testing Strategy](docs/development/testing.md) defines the required evidence for those contracts.
- [Future Considerations](docs/future/README.md) records non-binding work outside the current contract.

Future and draft material cannot change the v0.8.0 source contract.
