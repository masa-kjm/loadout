# Mutation-safety Review Evidence

The authoritative safety and recovery contracts are [File Links](../../../../docs/specs/file-link.md), [Lifecycle](../../../../docs/specs/lifecycle.md), [State and Recovery](../../../../docs/specs/state-and-recovery.md), and [Architecture Boundaries](../../../../docs/architecture/boundaries.md). Use this lens whenever a change can write, remove, replace, inspect for recovery, or persist state.

## Establish the operation contract

For each changed write path, identify its recorded precondition, post-condition, trusted roots, intended writes, commit point, dry-run result, and possible failure aftermath. Verify that the change preserves source-store contents, profiles, configuration, control files, unrelated targets, and prior Known state unless the applicable specification explicitly changes one of those contracts.

The baseline checks are:

- Destructive mutation is authorized only when durable Known state and a current no-follow observation both prove the expected owned file link.
- Unexpected regular files, directories, links, junctions, reparse points, and unsafe parents are neither followed, adopted, replaced, nor removed.
- A lexical path check is not containment proof. Existing source and target path components must satisfy the applicable no-follow containment contract.
- Apply uses a fresh executable Plan and successful preflight. The executor repeats containment, parent, target-kind, source, and ownership checks immediately before mutation without replanning.
- State records `running` before mutation. Known state changes only after the exact post-condition is verified and the update is atomically committed with `succeeded`.
- After an attempted mutation, post-mutation observation classifies the result as succeeded, failed, or uncertain. An operating-system return value alone is not sufficient.
- Dry-run is fully side-effect free, including locks, directories, temporary links, operation records, state, and cleanup.
- Cleanup may remove only the exact Loadout-owned temporary entry authorized by the recorded action. It must not remove user-visible artifacts.
- Recovery observes recorded facts and never resumes or reinterprets an old Plan. An uncertain operation blocks a new apply.

For a sequential apply, inspect operation progress, reports, Known-state commits, and each action's failure aftermath. A failure must not make an unprocessed action appear applied or alter a previously verified Known-state fact.

## Evidence to seek

Prefer tests that observe filesystem state, not merely an error return. For each applicable risk, seek evidence for the successful operation, a zero-mutation dry run, protected existing targets, containment failure, and a failure after an introduced write. State why a risk is not applicable rather than silently omitting it.
