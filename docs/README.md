# Loadout Documentation

This directory defines the v0.2 product.
It describes a resource-oriented local environment manager.

## Status and Authority

This documentation is being published in layers.
Each published document is authoritative only for the subject it owns.

- `architecture/` defines the v0.2 system model, responsibility boundaries, and non-negotiable architectural rules.
- `specs/` defines the v0.2 external contracts, including schemas, lifecycle behavior, ownership, path safety, and durable state.
- `development/` defines how the contracts are verified without redefining them.
- `future/` holds non-binding designs for work outside v0.2.0.
- `draft/` preserves exploratory material and is not an implementation source of truth.

## v0.2.0 Scope

v0.2.0 establishes the safe core for converging a composed profile to a local environment.
Its only resource implementation is a local-store source linked to a single-file target.
The core includes profile composition, validation, drift inspection, planning, conflict detection, state locking, safe application, and durable state recording.

Task resources, copy operations, directory resources, remote stores, profile parameters, imports, secret handling, ACL management, rollback, and parallel execution are outside v0.2.0.

## Documentation Rules

- Architecture documents define responsibility, dependency, and safety boundaries.
- Specification documents define observable behavior, schemas, and state transitions.
- Development documents define review and testing practice.
- Future documents may explore alternatives but must not alter v0.2.0 behavior.
- Draft documents may inform a decision but never override a published architecture or specification document.

User-visible documentation, code, and code comments are written in English.
