# Architecture Boundaries

## Purpose

These boundaries keep safety decisions in one place and prevent the command layer, resource implementations, and persistence code from making incompatible decisions.
They apply to every v0.2 implementation, including tests and future resource types.

## Decision Ownership

| Concern | Owner | Other layers must not do |
| --- | --- | --- |
| Command parsing, confirmation, formatting, and exit status | Command adapter | Reimplement lifecycle, ownership, or filesystem policy. |
| Declaration parsing, profile composition, path binding, and semantic validation | Resolver and validator | Inspect target state, mutate the filesystem, or write state. |
| Target and parent observation | Actual state inspector | Change the filesystem or treat an observation as durable ownership. |
| Desired/Known/Actual classification and action selection | Planner | Perform I/O, mutate state, invoke commands, or print. |
| Immediate precondition recheck, mutation, and post-condition verification | Executor | Create an unplanned action or reclassify an action after planning. |
| State lock, operation progress, Known state, and atomic commit | State repository | Delegate authoritative state writes to command or resource code. |
| Platform-specific path and filesystem primitives | Filesystem implementation | Decide ownership, desired state, or user-visible policy. |

## Planning and Execution

The planner is a pure decision boundary:

```text
Resolved Desired + Known + Actual -> Plan
```

The executor is an effect boundary:

```text
Resource Execution Plan + verified execution context -> effects + verification result
```

The executor must execute only actions present in the Plan.
It may reject an action when its immediate safety recheck fails, but it must not silently convert a rejected action into another action.
For example, a planned create must not become a replacement because an unexpected target appeared after planning.

## Ownership and Removal

Known state alone never authorizes a destructive filesystem action.
For the v0.2.0 file-link resource, removal or replacement is permitted only when both conditions hold:

1. Known state records Loadout's expected link for the resource.
2. Actual inspection confirms that the target is that expected link.

Any missing proof, wrong link, regular file, directory, symlinked parent, junction, reparse point, or other unexpected entry is a conflict or safety failure.
It must not be replaced, followed, or removed.

v0.2.0 has no forceful takeover of an unmanaged target.
Explicit transfer-of-ownership behavior, if ever introduced, requires its own specification and confirmation contract.

## Filesystem Mutation

Filesystem mutation is permitted only after a fresh executable plan and preflight checks.
The executor must revalidate containment, parent safety, target kind, and the action-specific ownership precondition immediately before mutation.

Path validation must account for symlinks on Unix and symlinks, junctions, reparse points, and case behavior on Windows.
A lexical path check is not containment proof.
The detailed algorithms and supported platform behavior belong to the file-link specification.

The executor must modify only the resolved target named by the action.
It must not write to Loadout control files, state files, lock files, profiles, or store contents as a side effect of materializing a resource.

## State and Failure Boundaries

The state repository is the only authority that writes durable Known state.
The executor must not make a successful mutation appear managed until its post-condition has been verified.

Apply records durable progress before a resource mutation and commits Known state only after verification.
If execution stops, a resource without a confirmed post-condition is uncertain rather than successful.
The next apply observes actual state and creates a new plan; it does not blindly resume an old plan.

v0.2.0 does not promise rollback of already verified resource actions.
Failure cleanup must not remove user-visible artifacts other than Loadout's own temporary files.

## Schema-Version Boundary

Every persisted control document has a schema version that identifies its structural and behavioral contract.
A normal lifecycle or inspection command must reject an unsupported version before it inspects a managed target, makes a planning decision, or writes durable state.
It must not ignore unknown ordering, ownership, recovery, or resource-effect data in order to continue.

Schema migration is outside the v0.2.0 executable surface.
When introduced, it must be an explicit operation rather than an implicit step of validation, planning, application, or inspection.
State migration must hold the state repository's exclusive lock and require `active_operation == null`.
An active or uncertain operation must be recovered and closed by the binary that implements its original state contract before migration.
The future migration protocol is described in [Schema Evolution and Migration](../future/schema-evolution-and-migration.md).

## Diagnostics and Errors

Core layers return structured diagnostics and errors.
Only the command adapter formats them for a terminal or maps them to an exit status.

A blocking diagnostic prevents mutation.
An error after a mutation is reported together with the durable operation status needed for a later recovery attempt.

If a dry-run mode is exposed, it must not create directories, acquire a mutating lock, write state, write operation records, create temporary files in managed paths, or otherwise mutate persistent or managed state.

## Extension Boundary

Future resource types must use the same ownership, inspection, planning, execution, verification, diagnostics, and state boundaries.
They may not add a shortcut from command code to filesystem mutation or from a resource handler to durable state.

The architecture does not require every future concept to be an abstraction today.
v0.2.0 implements the file-link lifecycle completely first and adds a shared abstraction only when multiple implemented resource types need it.
