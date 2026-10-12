# Specifications

These documents define the observable contracts.
They are authoritative for behavior, data, safety rules, and failure aftermath.
Architecture documents define responsibility boundaries; they do not replace these specifications.

## Documents

- [Configuration](configuration.md) defines machine-local runtime configuration, portable environment configuration, stores, and path syntax.
- [Profiles](profiles.md) defines profile discovery, profile composition, resource declarations, and semantic validation.
- [File Links](file-link.md) defines the `link` operation for a regular-file resource.
- [File Copies](file-copy.md) defines the `copy` operation, content ownership, and publication contract.
- [Directory Links](directory-link.md) defines the directory symbolic-link operation, its typed ownership effect, and its lifecycle transitions.
- [Lifecycle](lifecycle.md) defines validation, planning, preflight, application, deterministic ordering, and the Desired/Known/Actual transition table.
- [State and Recovery](state-and-recovery.md) defines durable Known state, operation records, locking, atomic commits, and crash recovery.
- [CLI](cli.md) defines the v0.6.0 command surface, confirmation behavior, and exit status classes.
- [Initialization](init.md) defines the v0.3.0 authoring command that creates an initial portable environment bundle.
- [Configuration Authoring](config-authoring.md) defines v0.3.0 configuration inspection, selection, and typed editing commands.
- [Inspection](inspection.md) defines the v0.6.0 read-only `status`, profile, and resource command contracts.

## Normative Language

The terms **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, and **MAY** describe the strength of a requirement.
An implementation that does not satisfy a MUST or MUST NOT requirement is not an implementation of the version that owns that requirement.

## v0.6.0 Baseline

v0.6.0 retains the v0.5.0 link, copy, authoring, and inspection contracts except where a v0.6.0 specification explicitly supersedes them.
DirectoryLink is a third closed resource effect; it does not relax ownership, no-follow observation, state-validation, locking, recovery, or mutation rules.

## Version Scope

Runtime configuration uses schema version `1` and profile documents use schema version `2`.
Portable environment configuration uses schema version `2`.
Durable state uses schema version `2`.
v0.5.0 commands reject v0.4 and unknown profile/state versions before relying on them; they provide no migration or old-schema reader.

v0.6.0 retains the existing runtime and portable environment configuration schema versions, while profiles use schema version `3` and durable state uses schema version `3`.
The [Current Profile Schema](profiles.md#current-profile-schema) and [Current State Schema](state-and-recovery.md#current-state-schema) replace the v0.5 profile/state readers.
They reject every version-2 or unknown profile/state document before target inspection, recovery, planning, state rewrite, or target mutation, without migration or compatibility decoding.
