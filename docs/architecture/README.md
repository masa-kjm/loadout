# Architecture

This directory defines the v0.2 system model and the boundaries that keep its planning, mutation, and durable-state responsibilities separate.
It is authoritative for architecture only; it does not define YAML schemas, CLI syntax, filesystem algorithms, or the state format.

## Documents

- [Overview](overview.md) defines the product model, core data flow, and the responsibilities of the major subsystems.
- [Boundaries](boundaries.md) defines ownership, mutation, state, and extension constraints that implementations must preserve.

## Architecture Commitments

The following commitments apply throughout v0.2:

- The planner derives a plan from Resolved Desired, Known, and Actual state without performing I/O or mutation.
- The executor performs only actions present in a plan and does not make a new planning decision.
- Filesystem mutation and durable state commits are separate responsibilities, connected by post-condition verification.
- A resource is never removed solely because it appears in Known state; ownership and the current filesystem entry must both satisfy the applicable contract.
- A command validates every persisted control-document schema version on which it depends before a target inspection, planning decision, or durable-state mutation relies on that document; a future migration operation is the only boundary allowed to transform an unsupported version.
- A future resource type may extend well-defined lifecycle boundaries, but it must not bypass ownership, state, or diagnostic boundaries.
