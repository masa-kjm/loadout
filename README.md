# Loadout

Loadout is a local environment manager built around explicit desired state, ownership-aware filesystem operations, and durable recovery.

## Features

- Link or copy files and link directories from local stores.
- Compose profiles with typed parameters, resource prerequisites, and stages.
- Validate and preview changes before applying them with ownership checks and crash recovery.
- Inspect managed state and drift with read-only commands.

## Installation

With Homebrew:

```sh
brew install masa-kjm/tap/loadout
```

Alternatively, install a release archive with the Unix or Windows installer:

```sh
curl -fsSL https://raw.githubusercontent.com/masa-kjm/loadout/main/scripts/install.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/masa-kjm/loadout/main/scripts/install.ps1 | iex
```

Both installers accept an exact release tag through `--version vX.Y.Z` or `-Version vX.Y.Z` and install below `~/.local/bin` or `%USERPROFILE%\.local\bin` by default. They download and verify the release archive's SHA-256 sidecar before installation.

## Basic Usage

From the directory where you keep your source files, create a Loadout configuration and select it for this machine:

```sh
loadout init
loadout config use ./.loadout/config.yaml
```

`init` creates `.loadout/config.yaml` and an empty `.loadout/profiles/base.yaml`. It configures the current directory as the `native` store. Add a source file named `example.txt` in this directory, then replace `base.yaml` with:

```yaml
schema_version: 6
id: base
resources:
  example:
    type: file
    properties:
      kind: file
      source:
        store: native
        path: example.txt
      target: ~/.loadout-example.txt
      operation: copy
```

Choose a target name that does not already exist. Validate the profile, review the plan, then apply it:

```sh
loadout validate
loadout plan
loadout apply
```

`config use` and `apply` ask for confirmation in an interactive terminal; add `--yes` for non-interactive use. To inspect drift later, run:

```sh
loadout diff
```

For an existing configuration, reusable profiles, ordering, and more commands, see the [User Guide](docs/user-guide.md).

## Documentation

The authoritative current-source contract is in [`docs/`](docs/README.md).

- [User Guide](docs/user-guide.md) walks through setup and everyday use.
- [Architecture](docs/architecture/README.md) defines system responsibilities and boundaries.
- [Specifications](docs/specs/README.md) define the observable contracts.
- [Testing Strategy](docs/development/testing.md) defines the required evidence for those contracts.
- [Future Considerations](docs/future/README.md) records non-binding work outside the current contract.
