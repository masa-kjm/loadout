# Task Resources

## Status

This is a non-binding design note.
Task resources are not part of v0.2.0 and this document defines no task schema or command behavior.

## Problem

Some local environment changes cannot be represented as a file link.
Examples include invoking a package-manager setup command, registering a shell integration, or running a tool-specific configuration script.

Unlike file links, arbitrary commands do not provide a filesystem fact that Loadout can safely interpret as ownership.
The design must therefore avoid claiming that a command is idempotent, reversible, or safe merely because it exits successfully.

## Candidate Model

A task would need separate declarations for:

- an observational predicate that reports whether the desired effect is satisfied;
- an apply operation;
- an optional remove operation; and
- explicitly recorded managed data needed to verify or remove a prior effect.

The observational predicate should have a fixed outcome model such as satisfied, unsatisfied, and inspection error.
It must be side-effect free.
An apply operation without a reliable predicate would need an explicit idempotency contract from its author, but that contract alone is not evidence of safe removal.

## Ownership and Removal Questions

Before task resources can be specified, the design must answer all of the following:

- What observation proves that a task effect belongs to this resource rather than to a user or another tool?
- When a task is removed from a profile, is a remove operation required, forbidden, or explicitly persistent?
- How are effects that cannot be proven safe to remove represented in a plan?
- Which values observed during apply must be stored as managed data for later verification or removal?
- How can a changed task definition update an existing effect without an unsafe remove-and-reapply sequence?

One possible direction is to store an immutable removal artifact and managed data only after apply succeeds and its post-condition is verified.
That artifact must include every helper file it needs, or the schema must require a self-contained removal command.

## Lifecycle Constraints

A task resource would still use the same boundaries as a file link:

```text
Declared -> Resolved Desired -> Actual -> Plan -> Execution -> Verification -> Known
```

The planner would not execute predicates or commands.
An inspector or task-specific observation adapter would produce typed Actual state.
The executor would run only planned commands and would not interpret a command result as ownership without the required post-condition.
The state repository would remain the only writer of durable Known state.

An interrupted task is especially sensitive.
If its final state cannot be proven from the recorded predicate and managed data, recovery must retain it as uncertain and must not automatically run it again.

## Security and Process Boundaries

The future schema should prefer direct process execution with an explicit executable and argument array.
Shell evaluation, inherited environment variables, working-directory selection, timeouts, standard-input handling, and output capture each need an explicit contract before support is added.

Task resources must not obtain direct access to state files or unrestricted control over other managed resources.
The design must also decide whether executable paths are resolved during planning, preflight, or execution and how a changed executable is diagnosed.

## Required Promotion Work

Promotion requires a final schema, a total Desired/Known/Actual transition table, recovery rules, and tests for satisfied, unsatisfied, failed, interrupted, changed-definition, missing-remover, and persistent-effect cases.
