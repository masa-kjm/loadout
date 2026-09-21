# Loadout Documentation

This directory contains the released v0.4.0 baseline and the v0.5.0 development contract.
The v0.5.0 documents define the contract to be implemented for the next release.
It describes a resource-oriented local environment manager.

## Status and Authority

This documentation is being published in layers.
Each published document is authoritative only for the subject it owns.

- `architecture/` defines the v0.5 system model, responsibility boundaries, and non-negotiable architectural rules.
- `specs/` defines the v0.5 external contracts, including schemas, lifecycle behavior, ownership, path safety, durable state, authoring commands, and inspection commands.
- `development/` defines how the contracts are verified without redefining them.
- `future/` holds non-binding designs for work outside v0.5.0.
- `draft/` preserves exploratory material and is not an implementation source of truth.

## v0.5.0 Contract Scope

v0.5.0 retains the v0.4.0 safe core for converging a composed profile to a local environment.
It supports a local-store regular file materialized as either a file symbolic link or a content-owned regular-file copy at a single-file target.
The core includes profile composition, validation, drift inspection, planning, conflict detection, state locking, safe application, and durable state recording.
It also provides narrow authoring commands: `init` creates a previously absent portable environment bundle, and `config` explicitly inspects or edits configuration without entering the resource lifecycle.
v0.4.0's read-only `status`, profile inspection, and resource inspection commands remain read-only in v0.5.0.
They expose declared, Known, and Actual information through explicit command scopes without acquiring the apply lock, recovering an operation, writing state, or changing a target.

Task resources, directory resources, remote stores, profile parameters, imports, secret handling, ACL management, rollback, and parallel execution are outside v0.5.0.
v0.5.0 deliberately has no migration command or compatibility reader for v0.4 profile or state schemas.

## Documentation Rules

- Architecture documents define responsibility, dependency, and safety boundaries.
- Specification documents define observable behavior, schemas, and state transitions.
- Development documents define review and testing practice.
- Future documents may explore alternatives but must not alter v0.5.0 behavior.
- Draft documents may inform a decision but never override a published architecture or specification document.

User-visible documentation, code, and code comments are written in English.
