# Architecture Overview

## Purpose

Loadout is a portable local environment manager.
It converges a machine toward the desired resource set produced by one composed root profile while leaving source assets in their native files and formats.

Loadout owns profile composition, resource lifecycle planning, local state, conflict detection, and the safe mutation of supported resources.
It does not reimplement package managers, runtime version resolution, secret management, or the side-effect guarantees of arbitrary commands.

## v0.2.0 Core

v0.2.0 establishes the lifecycle architecture with one supported resource implementation: a single file from a local store materialized as a link at a target path.
The architecture is intentionally prepared for additional resource types, but task resources, copy operations, directory resources, remote stores, and parameters are not part of the v0.2.0 executable surface.

The first implementation must prefer a narrow, complete file-link lifecycle over generic extension mechanisms.
In particular, v0.2.0 does not expose an external resource-plugin API.

## System Model

The architecture separates declarations, past confirmed effects, and current observations.

```text
Declared Configuration
        |
        v
Resolve and Validate
        |
        v
Resolved Desired ----+---- Known State Repository
                    |              |
                    |              v
                    +---- Actual State Inspector
                                   |
                                   v
                        Planner (pure decision)
                                   |
                                   v
                                  Plan
                                   |
                                   v
                         Executor and Filesystem
                                   |
                                   v
                         Verify and Commit State
```

The command layer invokes this flow, presents diagnostics, obtains confirmation when required, and maps results to output and exit status.
It does not implement resource ownership, path safety, planning, or durable-state decisions.
The read-only `diff` command uses the same Actual observation model to compare Known and Actual state without resolving Desired state or invoking the planner.

## Core Data Model

### Declared Configuration

Declared configuration is the user-authored configuration and profile input.
It includes profile IDs, includes, resource declarations, store references, and user-facing path syntax.
It is not an input to the planner.

### Resolved Desired

Resolved Desired is the canonical desired resource set for a selected root profile.
It has resolved includes, stable fully qualified resource IDs, bound stores, and normalized paths.
It contains no YAML syntax, store ID, home-directory shorthand, relative path, or profile-include traversal for downstream layers to interpret.

### Known

Known state is the durable record of resource effects that Loadout has successfully verified and committed.
It records enough applied facts to prove ownership before a later destructive operation.
Known state is evidence of a past operation, not proof that the current filesystem still has the expected entry.

### Actual

Actual state is the current observation of a resource target and its relevant parents.
For a file-link resource, the observation must distinguish at least a missing target, the expected link, a link to another target, a regular file, and an unsafe parent path.
Actual state is not persisted as an assertion of ownership.

### Plan

A Plan is an immutable, deterministic decision derived from Resolved Desired, Known, and Actual state.
It contains executable resource actions, their reasons, and blocking diagnostics.
Conflicts are diagnostics that make a plan non-executable; they are not executable actions.

### Resource Execution Plan

A Resource Execution Plan is the executor-ready payload for one planned action.
It identifies the resolved target, the expected precondition, the required post-condition, and the state update that becomes eligible only after successful verification.

## Lifecycle Flow

### Validate

Validation parses and resolves declarations, checks their semantic consistency, and reports diagnostics without mutating the managed environment.
It validates only what can be established from declarations and safe configuration resolution.

### Plan

Planning receives Resolved Desired, Known, and Actual state and produces a deterministic Plan.
The planner does not read the filesystem, access stores, mutate state, invoke commands, or render terminal output.

### Apply

Apply acquires the state lock before it observes mutable state for execution.
It resolves and validates the selected profile, obtains Known and Actual state, creates a fresh plan, runs preflight checks, records operation progress, executes the plan, verifies each result, and commits verified Known state.

The executor rechecks safety-sensitive filesystem preconditions immediately before mutation because the filesystem may have changed after planning.
If a recheck fails, the executor aborts the action; it does not reinterpret the action or select a replacement action.

### Determinism

Apply is sequential in v0.2.0.
Execution order must not depend on a YAML mapping iteration order or an implementation collection order.
The lifecycle specification will define the stable phase order and the fully qualified resource-ID ordering within each phase.
v0.2.0 does not expose resource IDs or declaration position as an ordering control.
A future dependency model may constrain action order, but independent actions must retain a canonical deterministic tie-breaker.

## Responsibilities

| Subsystem | Responsibility |
| --- | --- |
| Command adapter | Parse command input, request confirmation, render diagnostics and reports, and choose exit status. |
| Configuration loader | Locate and parse machine-local configuration and portable profiles. |
| Resolver and validator | Compose profiles, bind paths and stores, normalize declarations, and reject invalid desired state. |
| Actual state inspector | Observe managed targets and safety-relevant filesystem facts without mutation. |
| Planner | Classify Resolved Desired, Known, and Actual state into a deterministic Plan. |
| Executor | Perform planned actions, recheck immediate mutation safety, and verify post-conditions. |
| State repository | Own the state lock, operation records, durable Known state, and atomic commits. |
| Filesystem implementation | Perform platform-specific filesystem observation and mutation behind the executor's contract. |

## Resource-Type Evolution

Each resource type belongs to the same lifecycle: parse, resolve, inspect, plan, execute, verify, and commit.
Adding a type may add a handler at those boundaries, but it must not cause command code to duplicate planning or ownership decisions.
No handler may write durable state directly, render terminal output, or accept unresolved configuration syntax.

This is a controlled internal extension boundary, not a commitment to dynamic plugins.
New abstractions are introduced only when a second supported resource type needs the same boundary.

## Platform Isolation

The planner is platform-agnostic after resolution.
Platform-specific code is limited to path resolution, filesystem observation, link creation, replacement behavior, and durability primitives.
An unsupported or unsafe platform condition is a blocking diagnostic before a managed target is changed.

The specifications define the exact Unix and Windows behavior, including path containment and link semantics.
