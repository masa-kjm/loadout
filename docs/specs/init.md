# Initialization

## Scope

This specification defines the `init` authoring command introduced in v0.3.0.
It creates an initial portable environment bundle without selecting it as the machine-local default and without invoking the resource lifecycle.

## Command

```text
loadout init [--dry-run]
```

The command always addresses `.loadout` directly below the current working directory.
It accepts no path argument, does not read `loadout.yaml`, does not acquire the state lock, and does not read or write state.

`--dry-run` reports the directory that would be created and performs no filesystem mutation, including staging-directory creation.

## Generated Bundle

On success, `init` creates this complete directory tree:

```text
.loadout/
├── config.yaml
└── profiles/
    └── base.yaml
```

The generated `config.yaml` is a version-2 environment configuration that selects `base`, discovers `./profiles`, and declares the current directory as the `native` local store through `properties.path: ..`.
The generated `base.yaml` is a version-1 profile with ID `base` and an empty resource set.

The bundle contains Loadout control metadata.
Native source assets remain outside `.loadout` and Loadout lifecycle commands MUST NOT create, edit, remove, or otherwise mutate control files.
`init` is the narrow authoring exception: it creates only a previously absent initial bundle and never edits an existing control file.

The generated configuration is not automatically selected.
An operator selects it explicitly, for example with `loadout validate --config ./.loadout/config.yaml`.

## Existing Entries and Publication

If `.loadout` already exists as any entry kind, including a directory, regular file, symbolic link, junction, or other reparse point, `init` MUST reject it without following, replacing, or modifying it.

Before publication, Loadout creates a unique sibling staging directory, writes the complete bundle there, flushes the generated files where supported, re-opens and validates both YAML documents, and then publishes the directory with a platform primitive that does not replace an existing destination.
It rechecks that `.loadout` is absent immediately before this publication.
An external entry that wins the race is preserved.
After publication, Loadout re-opens both files and verifies their exact generated contents before reporting success.
As with other filesystem operations, an external writer can still substitute an entry after the final check; `init` does not claim atomic protection against that later change.

If staging creation, writing, validation, flushing, or publication fails before publication, `init` MUST NOT publish a partial `.loadout` bundle or mutate state, a store, native assets, runtime configuration, or an existing control entry.
If the post-publication check cannot prove the generated bundle, `init` MUST return status `1` and preserve the observed `.loadout` entry rather than deleting a possibly external entry.
Loadout MUST NOT delete a staging directory after a failure unless it can prove that the exact entry remains its own staging directory; an uncleaned staging directory is retained rather than risking deletion of an external entry.

## Exit Status and Output

`init` returns status `0` after successful creation or dry run.
It returns status `2` when `.loadout` already exists, an argument is invalid, or the platform lacks a no-replace directory-publication primitive.
It returns status `1` for I/O, durability, validation, or publication failures.

On success, output identifies the created control-directory path.
Dry-run output identifies the path that would be created.

## Non-Goals

`init` does not initialize version control, choose a remote, move or import native assets, edit an existing portable configuration, write `loadout.yaml`, alter the state location, install a store, fetch a remote source, or apply resources.
