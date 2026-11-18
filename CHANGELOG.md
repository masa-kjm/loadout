# Changelog

## v0.8.3

- Allow profiles to bind `source.path` from a string parameter.
- Allow parameterized includes to omit `as`, defaulting to the included profile ID and rejecting conflicting bindings under the same instance name.

## v0.8.2

- Replaced `migrate state` and `migrate profiles` with one confirmed `loadout migrate --config <path> [--to-schema 4|5] [--yes]` command.
- Added continuous schema-3-to-4-to-5 migration for mixed state and profile inventories, including target generation 4, complete preflight, confirmed-snapshot checks, and resumable partial publication.

## v0.8.1

- Added compact, deterministic lifecycle action-graph scheduling with fail-closed relation validation, durable applied lifecycle topology, and recovery-aware operation records.
- Added explicit schema-4-to-5 state and profile migration for existing v0.7.0 installations.

## v0.7.0

- Added composable schema-4 profiles with typed parameter bindings and stable resource identities for profile instances.
- Added explicit state-first schema-3-to-4 migration for profiles and durable Known state, including complete inventory preflight and safe publication.

## v0.6.0

- Added `directory`/`link` resources for local-store real directories, with typed DirectoryLink ownership, no-follow observation, deterministic lifecycle transitions, durable recovery, and native Unix and Windows conformance evidence.

## v0.5.0

- Added content-owned `file` copy resources with exact-byte fingerprint ownership, no-replace publication, verified staged replacement, link/copy effect handoff, and durable recovery classification.

## v0.4.0

- Added read-only `status`, `profile`, and `resource` inspection commands for declared profiles, Resolved Desired resources, validated Known records, active operations, and no-follow Actual observations.

## v0.3.0

- Added portable environment initialization and explicit configuration authoring commands.
- Added `config path`, `config list`, and `config get` for read-only configuration inspection.
- Added `config use` for validated machine-local portable configuration selection and `config set` for presentation-preserving edits of supported configuration fields.

## v0.2.0

- Rebuilt Loadout as an incompatible v0.2.0 release; v0.1 configuration, state, commands, resources, and behavior are not supported.
- Added composed profile configuration, validated local stores, deterministic planning, and conflict detection for local file-link resources.
- Added the `validate`, `diff`, `plan`, and `apply` commands, including confirmation and dry-run support.
- Added ownership-aware file-link create, remove, replace, relocate, and managed identity-handoff lifecycles that reject unmanaged target takeover.
- Added durable state, exclusive locking, atomic state commits, and recovery of interrupted operations.
