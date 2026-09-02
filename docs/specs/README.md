# Specifications

These documents define the observable v0.2.0 contracts.
They are authoritative for behavior, data, safety rules, and failure aftermath.
Architecture documents define responsibility boundaries; they do not replace these specifications.

## Documents

- [Configuration](configuration.md) defines machine-local runtime configuration, portable environment configuration, stores, and path syntax.
- [Profiles](profiles.md) defines profile discovery, profile composition, resource declarations, and semantic validation.
- [File Links](file-link.md) defines the only v0.2.0 resource type and its ownership and filesystem-safety contract.
- [Lifecycle](lifecycle.md) defines validation, planning, preflight, application, deterministic ordering, and the Desired/Known/Actual transition table.
- [State and Recovery](state-and-recovery.md) defines durable Known state, operation records, locking, atomic commits, and crash recovery.
- [CLI](cli.md) defines the v0.2.0 command surface, confirmation behavior, and exit status classes.

## Normative Language

The terms **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, and **MAY** describe the strength of a requirement.
An implementation that does not satisfy a MUST or MUST NOT requirement is not a v0.2.0 implementation.

## Version Scope

All schemas in this directory begin at version `1`.
