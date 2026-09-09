# Loadout

Loadout is a local environment manager built around explicit desired state, ownership-aware filesystem operations, and durable recovery.

## Status

v0.2.0 is a clean-break redesign and is not released yet.
The repository implements `validate`, `diff`, `plan`, and `apply` (including confirmation and dry run) as described in the [CLI specification](docs/specs/cli.md).
Platform support is incomplete: link creation remains enabled on all Unix builds under the [existing-create transition](docs/specs/file-link.md#existing-unix-create-transition), without filesystem-type filtering. Recorded native create evidence covers Linux/local ext4; macOS/APFS creation remains unverified. Other combinations are not claimed supported merely because creation is enabled. Guarded replacement and removal are rejected during preflight; Windows mutation support remains unfinished.
The installer is not implemented yet.
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

## Documentation

The authoritative v0.2 documentation is in [`docs/`](docs/README.md).

- [Architecture](docs/architecture/README.md) defines system responsibilities and boundaries.
- [Specifications](docs/specs/README.md) define the v0.2.0 observable contracts.
- [Testing Strategy](docs/development/testing.md) defines the required evidence for those contracts.
- [Future Considerations](docs/future/README.md) records non-binding work outside v0.2.0.

`docs/architecture/` and `docs/specs/` are authoritative for v0.2.0.
Future and draft material may inform a later design, but it cannot change a published v0.2.0 contract.
