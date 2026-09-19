# Specifications

These documents define the observable v0.4.0 contracts.
They are authoritative for behavior, data, safety rules, and failure aftermath.
Architecture documents define responsibility boundaries; they do not replace these specifications.

## Documents

- [Configuration](configuration.md) defines machine-local runtime configuration, portable environment configuration, stores, and path syntax.
- [Profiles](profiles.md) defines profile discovery, profile composition, resource declarations, and semantic validation.
- [File Links](file-link.md) defines the only v0.3.0 resource type and its ownership and filesystem-safety contract.
- [Lifecycle](lifecycle.md) defines validation, planning, preflight, application, deterministic ordering, and the Desired/Known/Actual transition table.
- [State and Recovery](state-and-recovery.md) defines durable Known state, operation records, locking, atomic commits, and crash recovery.
- [CLI](cli.md) defines the v0.4.0 command surface, confirmation behavior, and exit status classes.
- [Initialization](init.md) defines the v0.3.0 authoring command that creates an initial portable environment bundle.
- [Configuration Authoring](config-authoring.md) defines v0.3.0 configuration inspection, selection, and typed editing commands.
- [Inspection](inspection.md) defines the v0.4.0 read-only `status`, profile, and resource command contracts.

## Normative Language

The terms **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, and **MAY** describe the strength of a requirement.
An implementation that does not satisfy a MUST or MUST NOT requirement is not a v0.4.0 implementation.

## Retained v0.3.0 Baseline

v0.4.0 retains the v0.3.0 core contracts unchanged unless a v0.4.0 specification explicitly supersedes a rule.
Accordingly, every normative v0.3.0 requirement in Configuration, Profiles, File Links, Lifecycle, State and Recovery, Initialization, and Configuration Authoring remains a v0.4.0 requirement.
The v0.4.0 [Inspection](inspection.md) specification adds read-only commands; it does not relax or replace the retained ownership, no-follow observation, state-validation, locking, recovery, schema, or mutation rules.
References to v0.3.0 within a retained specification identify the origin and schema generation of that unchanged contract, not an exclusion from v0.4.0.

## Version Scope

Runtime configuration and profile documents use schema version `1`.
Portable environment configuration uses schema version `2`.
