# Architecture

This directory defines the system model and the boundaries that keep its planning, mutation, durable-state, authoring, and inspection responsibilities separate.
It is authoritative for architecture only; it does not define YAML schemas, CLI syntax, filesystem algorithms, or the state format.

## Documents

- [Overview](overview.md) defines the product model, core data flow, and the responsibilities of the major subsystems.
- [Boundaries](boundaries.md) defines ownership, mutation, state, and extension constraints that implementations must preserve.

## Architecture Commitments

The following commitments apply:

- The planner derives a plan from Resolved Desired, Known, and Actual state without performing I/O or mutation.
- The executor performs only actions present in a plan and does not make a new planning decision.
- Filesystem mutation and durable state commits are separate responsibilities, connected by post-condition verification.
- A resource is never removed solely because it appears in Known state; ownership and the current filesystem entry must both satisfy the applicable contract.
- A normal command validates every persisted control-document schema version on which it depends before a target inspection, planning decision, recovery action, or durable-state mutation relies on that document; v0.7 rejects schema 3 and unsupported versions and provides no transformation path.
- The explicit migration authoring path is isolated from normal readers, validates its complete schema-3 inventory, and raises the schema-4 state barrier before it converts profiles.
- The migration facade selects only explicitly registered private source-to-target routes; `schema_3_to_4` is the only supported route in v0.7, and adding or retiring a route cannot widen a normal reader.
- A future resource type may extend well-defined lifecycle boundaries, but it must not bypass ownership, state, or diagnostic boundaries.
- Inspection queries use resolved declarations, validated Known state, and no-follow Actual observations without invoking planning, execution, recovery, or durable-state mutation.
