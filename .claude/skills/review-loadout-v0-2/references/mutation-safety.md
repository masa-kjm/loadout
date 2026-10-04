# Mutation-safety Review Evidence

The authoritative safety and recovery contracts are [File Links](../../../../docs/specs/file-link.md), [File Copies](../../../../docs/specs/file-copy.md), [Lifecycle](../../../../docs/specs/lifecycle.md), [State and Recovery](../../../../docs/specs/state-and-recovery.md), and [Architecture Boundaries](../../../../docs/architecture/boundaries.md). Use this lens whenever a change can write, remove, replace, inspect for recovery, or persist state.

## Establish the operation contract

For each changed write path, identify its recorded precondition, post-condition, trusted roots, intended writes, commit point, dry-run result, and possible failure aftermath. Verify that the change preserves source-store contents, profiles, configuration, control files, unrelated targets, and prior Known state unless the applicable specification explicitly changes one of those contracts.

The baseline checks are:

- Destructive mutation is authorized only when durable Known state and a current no-follow observation both prove the expected owned effect: an exact link target for a link or a regular-file kind and exact byte fingerprint for a copy.
- Reject observed unexpected entries or unsafe parents before the next mutation step. Review the [external concurrency limits](../../../../docs/specs/file-link.md#external-filesystem-concurrency); do not require atomic entry identity or claim every after-recheck substitution is detected.
- A lexical path check is not containment proof. Existing source and target path components must satisfy the applicable no-follow containment contract.
- Apply uses a fresh executable Plan and successful preflight. The executor repeats containment, parent, target-kind, source, and ownership checks immediately before mutation without replanning.
- State records running before any mutation, or before the final ownership verification for a state-only action.
- After an attempted mutation, post-mutation observation classifies the result as succeeded, failed, or uncertain. An operating-system return value alone is not sufficient.
- Dry-run is fully side-effect free, including locks, directories, temporary artifacts, operation records, state, and cleanup.
- Cleanup may remove only the temporary entry authorized by the recorded action. A staged copy additionally requires durable `temporary_staged` and the current no-follow pathname, kind, fingerprint, and safe-parent proof; it must not remove a temporary without that fact or a user-visible artifact.
- Recovery observes recorded facts and never resumes or reinterprets an old Plan. An uncertain operation blocks a new apply.

For a sequential apply, inspect operation progress, reports, Known-state commits, and each action's failure aftermath. A failure must not make an unprocessed action appear applied or alter a previously verified Known-state fact.

For ordinary link replacement, check target and temporary predicates again after temporary creation and before atomic replacement. For staged copy replacement and `link -> copy`, verify and durably record `temporary_staged` before old-effect removal, then recheck the old effect and temporary before no-replace publication. For `copy -> link`, do not require a replacement temporary; recheck the old copy immediately before removal and use final-link no-replace creation. Verify required declared-path association rather than only handle-local observations. Require `publication_attempted` with the final postcondition before a sequential copy or effect handoff recovers as succeeded; a safely observed missing final target is failed, while unavailable facts remain uncertain.

## Evidence to seek

Prefer tests that observe filesystem state, not merely an error return. For each applicable risk, seek evidence for the successful operation, a zero-mutation dry run, protected existing targets, containment failure, and a failure after an introduced write. State why a risk is not applicable rather than silently omitting it.
