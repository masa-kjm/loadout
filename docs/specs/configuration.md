# Configuration Specification

## Scope

This specification defines the machine-local runtime configuration, the portable environment configuration, local stores, and path syntax for v0.2.0.
It does not define profile files or resource behavior; see [Profiles](profiles.md) and [File Links](file-link.md).

## Runtime Locations

Loadout uses the following platform locations when a path is not supplied explicitly:

| Platform | Runtime configuration directory | State directory |
| --- | --- | --- |
| Linux and WSL | `$XDG_CONFIG_HOME/loadout`, or `~/.config/loadout` | `$XDG_STATE_HOME/loadout`, or `~/.local/state/loadout` |
| Windows | `%APPDATA%\\loadout` | `%LOCALAPPDATA%\\loadout` |

The runtime configuration file is `loadout.yaml` in the runtime configuration directory.
The default portable environment configuration file is `config.yaml` in the same directory.
The state directory is owned by Loadout and is not configurable by an environment configuration file.

## Runtime Configuration

`loadout.yaml` is machine-local.
It selects the portable environment configuration and must not contain resources, profiles, or state.

```yaml
schema_version: 1
config_path: /home/example/dotfiles/loadout/config.yaml
```

`schema_version` is REQUIRED and must be `1`.
`config_path` is optional.
When omitted, Loadout uses the default portable environment configuration file.

The path is resolved relative to the directory containing `loadout.yaml` when it is relative.
An absolute path and a home-relative path beginning with `~/` are also allowed.
Unknown fields are errors.

## Environment Configuration

The portable environment configuration identifies profiles and local stores.
It may be stored in any repository and is selected by `loadout.yaml` or the CLI `--config` option.

For `--config`, a relative path is resolved from the current working directory.
An absolute path and a home-relative path beginning with `~/` are also allowed.
The CLI option takes precedence over `loadout.yaml`.

```yaml
schema_version: 1
default_profile: workstation

profile_discovery:
  paths:
    - ./profiles

stores:
  dotfiles:
    type: local
    path: .
```

`schema_version`, `profile_discovery`, and `stores` are REQUIRED.
`default_profile` is optional.
Unknown fields are errors at every object level.

### `profile_discovery`

`profile_discovery.paths` is a non-empty ordered list of directories.
Each path is resolved relative to the environment configuration file when it is relative.
Each resolved directory must exist and be a directory.

Loadout discovers regular files with a `.yaml` extension directly in each directory.
Discovery is not recursive, and Loadout does not follow a symlink, junction, or reparse point while scanning a discovery directory.
All discovered profile IDs must be unique across every discovery directory.

The path order does not establish override priority.
A duplicate profile ID is an error that reports every defining file.

### `stores`

`stores` is an object keyed by store ID.
A store ID uses the same identifier grammar as a profile ID.
The only v0.2.0 store type is `local`.

```yaml
stores:
  dotfiles:
    type: local
    path: ~/src/dotfiles
```

For `type: local`, `path` is REQUIRED and identifies an existing directory.
The store path may be absolute, home-relative, or relative to the environment configuration file.
It may contain `..` because it is a configuration-level root path.

The resolved store root is used only to read source assets.
Loadout MUST NOT modify its contents while resolving, planning, applying, or recovering a v0.2.0 resource.

## Path Syntax

Configuration-level paths are `config_path`, `profile_discovery.paths[*]`, and `stores.*.path`.
They accept absolute paths, `~/` paths, and paths relative to the environment configuration file or runtime configuration file as specified above.
Environment variables other than the leading `~/` form are not expanded.

Resource-local paths have stricter rules and are defined by the File Links specification.
A lexical path-prefix comparison is never sufficient to prove filesystem containment.

## Default Profile

When `default_profile` is set, it must equal a discovered profile ID.
It supplies the root profile for `validate`, `plan`, and `apply` when their optional positional profile ID is omitted.
`validate --all` does not use `default_profile`.
It does not record an active profile and does not alter profile composition.
