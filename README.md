# Loadout

Loadout is a local environment manager built around explicit desired state, ownership-aware filesystem operations, and durable recovery.

## Status

v0.2.0 is a clean-break redesign and is not released yet.
Its architecture and specifications are complete enough to guide the first implementation, but the repository does not yet provide an implemented v0.2.0 command interface or installer.
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

Loadout never adopts, removes, or replaces an unmanaged target through the normal lifecycle.
A destructive action is permitted only when durable Known state and a current no-follow filesystem observation both prove the required ownership condition.

## Documentation

The authoritative v0.2 documentation is in [`docs/`](docs/README.md).

- [Architecture](docs/architecture/README.md) defines system responsibilities and boundaries.
- [Specifications](docs/specs/README.md) define the v0.2.0 observable contracts.
- [Testing Strategy](docs/development/testing.md) defines the required evidence for those contracts.
- [Future Considerations](docs/future/README.md) records non-binding work outside v0.2.0.

`docs/architecture/` and `docs/specs/` are authoritative for v0.2.0.
Future and draft material may inform a later design, but it cannot change a published v0.2.0 contract.
