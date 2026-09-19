# Loadout Documentation

This directory contains the released v0.3.0 baseline and the v0.4.0 contract under development.
The v0.4.0 documents define required behavior for that release; they do not claim that an unreleased command is installed or available.
It describes a resource-oriented local environment manager.

## Status and Authority

This documentation is being published in layers.
Each published document is authoritative only for the subject it owns.

- `architecture/` defines the v0.4 system model, responsibility boundaries, and non-negotiable architectural rules.
- `specs/` defines the v0.4 external contracts, including schemas, lifecycle behavior, ownership, path safety, durable state, authoring commands, and inspection commands.
- `development/` defines how the contracts are verified without redefining them.
- `future/` holds non-binding designs for work outside v0.4.0.
- `draft/` preserves exploratory material and is not an implementation source of truth.

## v0.4.0 Contract Scope

v0.4.0 retains the v0.3.0 safe core for converging a composed profile to a local environment.
Its only resource implementation is a local-store source linked to a single-file target.
The core includes profile composition, validation, drift inspection, planning, conflict detection, state locking, safe application, and durable state recording.
It also provides narrow authoring commands: `init` creates a previously absent portable environment bundle, and `config` explicitly inspects or edits configuration without entering the resource lifecycle.
v0.4.0 adds read-only `status`, profile inspection, and resource inspection commands.
They expose declared, Known, and Actual information through explicit command scopes without acquiring the apply lock, recovering an operation, writing state, or changing a target.

Task resources, copy operations, directory resources, remote stores, profile parameters, imports, secret handling, ACL management, rollback, and parallel execution are outside v0.4.0.

## Documentation Rules

- Architecture documents define responsibility, dependency, and safety boundaries.
- Specification documents define observable behavior, schemas, and state transitions.
- Development documents define review and testing practice.
- Future documents may explore alternatives but must not alter v0.4.0 behavior.
- Draft documents may inform a decision but never override a published architecture or specification document.

User-visible documentation, code, and code comments are written in English.
