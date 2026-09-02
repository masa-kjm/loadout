# Mutation and Recovery Test Design

The authoritative contracts are [File Links](../../../../docs/specs/file-link.md), [Lifecycle](../../../../docs/specs/lifecycle.md), and [State and Recovery](../../../../docs/specs/state-and-recovery.md).

For every new or changed mutation path, design evidence for the applicable cases:

- successful mutation and exact no-follow post-condition;
- rejected ownership, target-kind, source, containment, parent-safety, validation, preflight, or capability condition with no target mutation;
- executor recheck after a change between planning and mutation;
- durable `running` progress before mutation and no Known-state update before verified post-condition;
- failure after mutation classified from the recorded precondition and post-condition as succeeded, failed, or uncertain; and
- recovery of recorded `pending`, `running` with proven post-condition, `running` with retained precondition, and unprovable or unsafe observations.

For dry run, snapshot the target tree, state directory, store, configuration, and control files before and after. The snapshots must be identical. Do not overlook lock files, operation records, created directories, or temporary replacement links.

For replacement, include the action-local temporary sibling path in the design. Test that only the exact recorded temporary link is eligible for cleanup; an unexpected, unsafe, or unremovable temporary entry leaves the action uncertain.

Use real temporary directories. Never use a real home directory, XDG or AppData directory, source store, or repository state directory. Assert the retained filesystem and state aftermath, not only an error return.
