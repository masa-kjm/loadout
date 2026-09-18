# Changelog

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
