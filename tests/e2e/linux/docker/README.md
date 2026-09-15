# Linux Docker Runner

This runner builds the current v0.2 binary inside Docker and executes the CLI against a disposable home, configuration, state, and local-store tree.

## Requirements

- Docker with BuildKit support
- Network access for the first Rust base-image and dependency download

## Usage

From the repository root:

```bash
./tests/e2e/linux/docker/test.sh all
./tests/e2e/linux/docker/test.sh validate
./tests/e2e/linux/docker/test.sh apply
./tests/e2e/linux/docker/test.sh shell
```

`all` runs the smoke flow: validate, plan, apply, repeated apply, and diff. The container is removed after each run. The fixture is copied from `tests/fixtures/config/valid/`; no host configuration, state directory, or home directory is used.

`shell` opens an interactive container after preparing the same isolated environment. Inside it, run `loadout validate`, `loadout plan`, `loadout apply --yes`, or `loadout diff` manually.
The environment is removed when the shell exits.
