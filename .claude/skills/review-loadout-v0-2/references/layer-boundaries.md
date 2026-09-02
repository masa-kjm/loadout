# Layer-boundary Review Evidence

The authoritative boundary definitions are [Architecture Overview](../../../../docs/architecture/overview.md) and [Architecture Boundaries](../../../../docs/architecture/boundaries.md). Use this reference to identify the evidence a review needs; it is not a second architecture specification.

## Review scope

The current package has a binary crate for the command adapter and a library crate for core implementation. The filesystem layout may evolve, but the responsibility boundary is stable.

Check that command parsing, confirmation, terminal rendering, and exit-status mapping remain in the command adapter. The command adapter must call core lifecycle operations rather than reproduce validation, ownership, path safety, planning, state, recovery, dry-run, or platform decisions.

Check that core layers retain the responsibility split defined by the architecture:

- loaders parse only declarations and machine-local configuration;
- resolvers and validators produce resolved typed models without target inspection or mutation;
- inspectors observe Actual state without mutation or ownership adoption;
- planners receive only Resolved Desired, Known, and Actual state and perform no I/O;
- executors perform only planned actions, immediate safety rechecks, and post-condition verification;
- the state repository exclusively owns locks, operation records, Known state, and atomic commits; and
- filesystem implementations contain platform primitives, not user-visible policy or planning decisions.

The current library target is not a supported public Rust API. Do not request a new public export merely to test an implementation detail or make a command adapter convenient. Public API additions require an explicit external-contract reason.

## Evidence to seek

- The complete path from changed CLI input to the owning core operation.
- No duplicate safety or ownership decision in command code.
- Structured core diagnostics and result data sufficient for the CLI to format the intended user contract without core printing terminal output.
- Tests at the owner of the changed behavior, plus CLI-level evidence when observable command behavior changes.

Add a new review rule here only after it becomes a stable, reusable concern. Link to the architecture source of truth instead of copying its module inventory or dependency graph.
