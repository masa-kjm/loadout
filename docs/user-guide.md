# User Guide

This guide walks through a first setup and common commands. The [specifications](specs/README.md) define the exact behavior and safety rules.

## Start a New Configuration

Run these commands from the directory that holds your source files, such as a dotfiles repository:

```sh
loadout init
loadout config use ./.loadout/config.yaml
```

`init` creates `.loadout/config.yaml` and an empty `.loadout/profiles/base.yaml`. The generated configuration names the current directory as the `native` local store and selects `base` as the default profile. `config use` selects that portable configuration for this machine, so later commands can omit `--config`. It asks for confirmation in an interactive terminal; use `--yes` in a non-interactive script. See [Initialization](specs/init.md) and [Configuration Authoring](specs/config-authoring.md).

You can check the selected configuration with `loadout config path`. If you prefer not to select one for the machine, pass `--config ./.loadout/config.yaml` to declaration commands instead.

## Manage a First File

Place a regular file named `example.txt` beside `.loadout/`, then put this declaration in `.loadout/profiles/base.yaml`:

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

The source path is relative to the store root. The target is below your home directory and must have an existing parent directory. Choose a target that does not already contain an unmanaged entry; Loadout does not take ownership of one automatically. A file can use `operation: link` instead, and a real directory can use `kind: directory` with `operation: link`. See [Profiles](specs/profiles.md), [File Copies](specs/file-copy.md), [File Links](specs/file-link.md), and [Directory Links](specs/directory-link.md).

Review and apply the declaration:

```sh
loadout validate
loadout plan
loadout apply
```

`validate` checks the selected root and its included profiles, including declarations and sources. Pass a profile ID, such as `loadout validate base`, to select another root; without one, Loadout uses the configured default. `validate --all` is invalid. `plan` previews the actions without changing the target or state. `apply` checks the plan again before making changes and asks for confirmation in an interactive terminal. Use `loadout apply --dry-run` for another read-only preview or `loadout apply --yes` in a non-interactive script. See [Lifecycle](specs/lifecycle.md) and [CLI](specs/cli.md).

## Use an Existing Source Directory

You can keep the portable configuration separate from the source store. For example:

```text
loadout-config/
├── config.yaml
└── profiles/
    └── workstation.yaml

~/src/dotfiles/
└── gitconfig
```

`loadout-config/config.yaml` can point to that store:

```yaml
schema_version: 2
default_profile: workstation
profile_discovery:
  paths: [./profiles]
stores:
  dotfiles:
    type: local
    properties:
      path: ~/src/dotfiles
```

Define a resource in `loadout-config/profiles/workstation.yaml` with `source: { store: dotfiles, path: gitconfig }`, then select the configuration from `loadout-config/` with `loadout config use ./config.yaml`. Configuration-relative paths such as `./profiles` resolve from `config.yaml`; resource source paths resolve from their store root. See [Configuration](specs/configuration.md).

## Reuse Profiles and Set Order

A reusable profile can declare typed parameters. For example, `.loadout/profiles/shared-file.yaml` can use strings for its source and target paths:

```yaml
schema_version: 6
id: shared-file
parameters:
  source-path: string
  target-path: string
resources:
  config:
    type: file
    properties:
      kind: file
      source: { store: native, path: { parameter: source-path } }
      target: { parameter: target-path }
      operation: copy
```

A root profile can include it twice with different bindings:

```yaml
schema_version: 6
id: base
stages:
  source: { after: [] }
  destination: { after: [source] }
includes:
  - id: shared-file
    stage: source
    with: { source-path: git/work, target-path: ~/.gitconfig-work }
  - id: shared-file
    as: personal
    stage: destination
    with: { source-path: git/personal, target-path: ~/.gitconfig-personal }
resource_dependencies:
  - { before: shared-file/config, after: personal/config }
resources: {}
```

Create both source files in the store before validating. Each parameter reference occupies the whole YAML value. `as` defaults to the included profile ID; give instances different names when their bindings differ. Only the selected root declares `stages`. Each include's `stage` places its whole subtree in that phase, overriding a phase declared deeper in that subtree. Here, `resource_dependencies` also requires `personal/config` to follow `shared-file/config`; its references follow include aliases relative to `base`. See [Parameters and References](specs/profiles.md#parameters-and-references), [Includes](specs/profiles.md#includes), and [Orchestration](specs/profiles.md#schema-6-orchestration).

As a separate example, use `depends_on` for one resource that must follow another. Use stages when a group must follow another group. This profile applies `git-ignore` after `git-config`, then applies `editor-config` after the `files` stage:

```yaml
schema_version: 6
id: base
stages:
  files: { after: [] }
  editor: { after: [files] }
resources:
  git-config:
    type: file
    stage: files
    properties:
      kind: file
      source: { store: native, path: git/config }
      target: ~/.loadout-gitconfig
      operation: copy
  git-ignore:
    type: file
    stage: files
    depends_on: [git-config]
    properties:
      kind: file
      source: { store: native, path: git/ignore }
      target: ~/.loadout-gitignore
      operation: copy
  editor-config:
    type: file
    stage: editor
    properties:
      kind: file
      source: { store: native, path: editor/config }
      target: ~/.loadout-editorconfig
      operation: copy
```

`depends_on` names a resource in the same resolved instance. For a dependency between included instances, declare `resource_dependencies` in a parent whose include closure contains both endpoints. Loadout orders applicable actions according to these relationships; YAML declaration order does not control execution order. `plan`, dry-run apply, and Desired-resource inspection show each resource's effective phase and its source, plus normalized direct dependencies and their declaration sources. Ordering-only changes do not create resource actions or rewrite state. See [Orchestration](specs/profiles.md#schema-6-orchestration) and [Execution Order](specs/lifecycle.md#execution-order).

If `plan` reports that new ordering contradicts recorded ordering, read its old and proposed relations and the complete rebuild set. Remove every resource in that set from Desired, apply the stale-removal plan until every member and its old ordering facts are gone, then restore the new declarations and run `plan` and `apply` again. Finish any failed or uncertain operation through the recorded-state recovery or retry path before treating the rebuild as complete. A read-only `plan` or dry run does not recover an operation; a normal `apply` may first recover an earlier operation and reports those effects separately. See [Recorded-order Conflicts](specs/lifecycle.md#recorded-order-conflicts).

## Inspect and Maintain

```sh
loadout status
loadout diff
loadout profile list
loadout resource list
loadout resource list --known
```

`status` compares desired, recorded, and observed resources. `diff` reports drift for recorded resources without reading the selected profile. The list commands show profile declarations, desired resources, or recorded resources. These commands do not repair targets; review a fresh `plan` before applying changes. See [Inspection](specs/inspection.md).

If your existing profile or state files use an older supported schema, run `loadout migrate --config <path>` before normal lifecycle commands. Migration previews the complete document inventory and any broader phase ordering before confirmation. A schema-5 state can advance to schema 6 only without an active operation or recorded topology; see [Migration](specs/migration.md).
