# Loadout Documentation

This directory contains the released v0.4.0, v0.5.0, and v0.7.0 baselines and the current v0.8.0 contract.
It describes a resource-oriented local environment manager.

## Status and Authority

This documentation is being published in layers.
Each published document is authoritative only for the subject it owns.

- `architecture/` defines the v0.8 system model, responsibility boundaries, and non-negotiable architectural rules.
- `specs/` defines the v0.8 external contracts, including schemas, lifecycle behavior, ownership, path safety, durable state, authoring commands, migration, and inspection commands.
- `development/` defines how the contracts are verified without redefining them.
- `future/` holds non-binding designs for work outside v0.8.0.
- `draft/` preserves exploratory material and is not an implementation source of truth.

## v0.8.0 Contract Scope

v0.8.0 retains the v0.7.0 safe core for converging a composed profile to a local environment.
It supports a local-store regular file materialized as either a file symbolic link or a content-owned regular-file copy at a single-file target, plus a local-store real directory materialized as a directory symbolic link.
The core includes profile composition, validation, drift inspection, planning, conflict detection, state locking, safe application, and durable state recording.
It also provides narrow authoring commands: `init` creates a previously absent portable environment bundle, `config` explicitly inspects or edits configuration without entering the resource lifecycle, and `migrate` explicitly moves a supported v0.7 environment to the v0.8 profile and state schemas.
v0.4.0's read-only `status`, profile inspection, and resource inspection commands remain read-only in v0.8.0.
They expose declared, Known, and Actual information through explicit command scopes without acquiring the apply lock, recovering an operation, writing state, or changing a target.

Profile parameters are closed typed values bound explicitly at include sites, and each resolved profile instance has a stable resource-identity namespace.
Task resources, directory copy, tree synchronization, remote stores, imports, secret handling, ACL management, rollback, and parallel execution are outside v0.8.0.
Schema-5 profiles add stable instance-scoped stages and canonical direct resource prerequisites. The planner turns those resolved facts and Applied Lifecycle Topology into one compact immutable action graph and deterministic sequential schedule. Normal lifecycle and inspection readers reject schema 4 and every unsupported profile or state schema before target observation, recovery, planning, or rewrite. Only the explicit state-first schema-4-to-5 migration operation may parse schema-4 inputs.

## Documentation Rules

- Architecture documents define responsibility, dependency, and safety boundaries.
- Specification documents define observable behavior, schemas, and state transitions.
- Development documents define review and testing practice.
- Future documents may explore alternatives but must not alter v0.8.0 behavior.
- Draft documents may inform a decision but never override a published architecture or specification document.

User-visible documentation, code, and code comments are written in English.
