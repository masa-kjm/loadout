# Mutation and Recovery Test Design

The authoritative contracts are [File Links](../../../../docs/specs/file-link.md), [File Copies](../../../../docs/specs/file-copy.md), [Lifecycle](../../../../docs/specs/lifecycle.md), and [State and Recovery](../../../../docs/specs/state-and-recovery.md).

For every new or changed mutation path, design evidence for the applicable cases:

- successful mutation and exact no-follow post-condition;
- rejected ownership, target-kind, source, containment, parent-safety, validation, preflight, or capability condition with no target mutation;
- executor recheck after a change between planning and mutation;
- durable running progress before any mutation, or before the final verification that authorizes a state-only action, and no Known-state update before the required post-condition is verified;
- failure after mutation classified from the recorded precondition and post-condition as succeeded, failed, or uncertain; and
- recovery of recorded `pending`, `running` with proven post-condition, `running` with retained precondition, and unprovable or unsafe observations.

For dry run, snapshot the target tree, state directory, store, configuration, and control files before and after. The snapshots must be identical. Do not overlook lock files, operation records, created directories, or temporary artifacts.

For ordinary `link -> link` replacement and source-changing link ownership replacement, include the action-local temporary link sibling path in the design. Test that only the exact recorded temporary link is eligible for cleanup; an unexpected, unsafe, or unremovable temporary entry leaves the action uncertain.

For sequential `replace_copy` and `link -> copy`, include a staged regular-file temporary. Test its no-follow kind and fingerprint verification, durable `temporary_staged` persistence before old-effect removal, and cleanup only when that fact and the current temporary proof both hold. Test durable `publication_attempted` together with the final postcondition for recovery success; a missing final target is failed, a safely observed different final entry is failed conflict, and unsafe or unavailable observation is uncertain.

For `copy -> link`, do not require a replacement temporary. Design evidence for final-link-source verification, immediate old-copy recheck, removal, no-replace final-link creation, and the same publication/recovery classification. This handoff does not inherit ordinary link replacement's atomic-publication or old-effect-preservation contract.

Use real temporary directories. Never use a real home directory, XDG or AppData directory, source store, or repository state directory. Assert the retained filesystem and state aftermath, not only an error return.
