# Testing Strategy

## Scope

This document defines how an implementation demonstrates conformance with the current architecture and specifications.
It does not add runtime behavior, change an error outcome, or replace a specification.

Every behavior change must identify its owning specification and add evidence at the narrowest test layer that can prove the contract.
An observable CLI or filesystem contract also requires an integration or acceptance test; a private unit test alone is not sufficient evidence.

## Read by Task

Start with the relevant row in the [Contract Matrix](#contract-matrix), then use the test layers below. The owning specification determines behavior; this page determines evidence.

| Change | Evidence sections to read |
| --- | --- |
| Profile schema, ordering, or planning | [Pure Domain Tests](#pure-domain-tests); [Profile Parameters and Migration Evidence](#profile-parameters-and-migration-evidence) for parameters, schema boundaries, or migration. |
| Resource mutation or ownership | [Filesystem Contract Tests](#filesystem-contract-tests), [State Repository Durability Tests](#state-repository-durability-tests), and [Executor and CLI Tests](#executor-and-cli-tests). |
| State commit or recovery | [State Repository Durability Tests](#state-repository-durability-tests) and [Executor and CLI Tests](#executor-and-cli-tests); include [Filesystem Contract Tests](#filesystem-contract-tests) when recovery may clean a target or temporary. |
| Platform capability or native behavior | [Platform Conformance](#platform-conformance), [Filesystem Contract Tests](#filesystem-contract-tests), and [Executor and CLI Tests](#executor-and-cli-tests). Include [State Repository Durability Tests](#state-repository-durability-tests) for recovery and [Platform Primitive Decisions](copy-platform-decision-record.md) for copy primitives. |
| CLI or read-only inspection | [Executor and CLI Tests](#executor-and-cli-tests); use [Profile Parameters and Migration Evidence](#profile-parameters-and-migration-evidence) for migration commands. |

## Test Layers

| Layer | Purpose | Typical evidence |
| --- | --- | --- |
| Pure domain tests | Prove normalization, validation, state classification, planning, and deterministic ordering without I/O. | A Desired/Known/Actual input produces the required Plan or blocking diagnostic. |
| Filesystem contract tests | Prove no-follow observation, containment, link operations, and the absence of forbidden mutations using a temporary filesystem. | The before and after entry kinds, link targets, paths, and directory contents. |
| State repository durability tests | Prove locking, atomic replacement, schema validation, operation progress, and recovery using fault injection. | The complete state before and after an interrupted commit or recovery attempt. |
| Executor integration tests | Prove the lifecycle across real temporary files, the state repository, and the filesystem implementation. | The planned action, resulting target, post-condition, Known state, and failure aftermath. |
| CLI acceptance tests | Prove arguments, confirmation, dry-run behavior, output categories, and exit-status classes. | Process status, stdout and stderr category, and isolated filesystem and state snapshots. |
| Platform conformance tests | Prove Unix and Windows behavior that cannot be established by a platform-neutral fake. | Actual symbolic-link, reparse-point, locking, and replacement behavior on the target platform. |

## Contract Matrix

The following matrix identifies the minimum evidence for each current contract. The sections below specify test boundaries and failure cases that the matrix does not detail.

| Contract owner | Required evidence |
| --- | --- |
| [Configuration](../specs/configuration.md) | Runtime and CLI configuration selection; path-base resolution; store-type-specific property validation and unknown-field rejection; duplicate profile IDs; store roots remain unchanged. |
| [Profiles](../specs/profiles.md) | Include order; cycle and missing-ID rejection; deduplication through multiple paths; fully qualified identity; target-collision rejection; deterministic ordering independent of input-map iteration. |
| [File Links](../specs/file-link.md) | Create, no-op, replace, relocate, remove, forget-missing, and managed identity-handoff outcomes; observed unmanaged-target rejection and the external-concurrency limits; wrong-link and regular-file conflicts; parent-escape rejection; source and target containment; no parent removal. |
| [File Copies](../specs/file-copy.md) | Create, no-op, replace, relocate, remove, forget-missing, link/copy handoff, source and target content drift, no-follow regular-file and exact-byte-fingerprint ownership, `create-no-replace` collision preservation, staged and direct-create failure aftermath, sequential copy replacement and handoff classification, and recovery without adoption of unmanaged final targets. |
| [Directory Links](../specs/directory-link.md) | Directory-source validation without descendant traversal; typed directory-link hash, Known ownership, and no-follow Actual classification; every DirectoryLink transition; unmanaged matching directory-link, file-link, junction/reparse point, regular directory/file, and unsafe-parent conflicts; mixed-effect handoff rejection; creation, replacement, relocation, removal, post-mutation failure, and recovery evidence on Unix and Windows. |
| [Current Profile Schema](../specs/profiles.md#current-profile-schema), [Current State Schema](../specs/state-and-recovery.md#current-state-schema), and [Migration](../specs/migration.md) | Normal profile and state readers strictly decode schema 6; older and unknown-version rejection before observation or side effects; strict orchestration/operation/topology decoding; migration-only older-generation decoding, route composition, 5→6 empty-topology eligibility, and preservation without adoption of old Known effects. |
| [Lifecycle](../specs/lifecycle.md) | Every Desired/Known/Actual table row; combined-DAG cycle and illegal-role rejection; compact barriers; deterministic fully-qualified-ID tie-breaking; ordering-only no-op; zero-mutation blocked plans; recheck rejection; and failure/uncertainty stop and skip classification. |
| [State and Recovery](../specs/state-and-recovery.md) | Corrupt graph/topology rejection before observation; durable graph-before-mutation; atomic action-authorized Known/topology commit; stale-owner recovery; durable-fact-only recovery; exclusive-lock contention; and no rollback of verified earlier actions. |
| [CLI](../specs/cli.md) | Positional root-profile selection; selected-root `validate` and `--all` argument rejection; `diff` Known-to-Actual reporting and zero mutation; `plan` and `apply` default-profile behavior; confirmation after successful preflight and before an operation record; non-interactive `--yes` requirement; dry-run zero mutation; all documented exit-status classes. |
| [Initialization](../specs/init.md) | A compiled-binary `init` creates the exact valid `.loadout` bundle; dry run changes no entry, state, store, or runtime configuration; every existing final entry kind is preserved; and staging write, validation, flush, publication, and external-collision failures never publish a partial bundle or replace an external entry. |
| [Configuration Authoring](../specs/config-authoring.md) | Read-only config commands leave configuration, state, store, and targets unchanged; typed-path and value rejection; complete-document validation; confirmation; runtime-selection creation and replacement; portable-configuration presentation preservation or rejection; publication failure and external-substitution aftermath; and Unix/Windows behavior. |
| [Inspection](../specs/inspection.md) | Every `status`, profile, and resource grammar and selection conflict; declaration/Desired/Known/Actual read-set boundaries; deterministic ordering; active-operation display for `status` and existing `diff` only; Desired-only, Known-only, definition-changed, expected, drifted, unsafe, and unavailable status rows; invalid declaration/state behavior; partial status report behavior; and snapshots proving no target, store, configuration, state, lock, operation, or directory mutation. |

## Pure Domain Tests

Pure domain tests prove canonical cross-instance references; direct-plus-stage combined-cycle rejection; closed action-role legality; compact barrier size and canonical validation; fully-qualified-ID scheduling under composition permutations; satisfied and unstaged no-op cases; and topology authority/delta selection without I/O.

Pure domain tests use resolved paths and typed observations only.
They must not parse YAML, access a store, inspect the host filesystem, acquire a lock, or serialize state.

At minimum, domain tests cover every row in the lifecycle transition table and assert both the action and its reason.
They also prove that resource ordering is stable when equivalent declarations are supplied in different mapping orders.
Canonical-hash fixtures use typed resolved values and assert the exact `definition_hash` and `desired_hash`, including deterministic resource ordering.
They prove that equal resolved definitions under distinct resource IDs have the same `definition_hash` and different `desired_hash` values.
Managed identity-handoff planning cases distinguish equal and differing resolved link targets and assert the recorded old and new identities and link targets.

Tests for a blocked plan must assert that the plan has blocking diagnostics and no executable action for the conflicting target.

DirectoryLink domain tests cover every row in [its transition table](../specs/directory-link.md#planning-transitions), including same-effect state-only and missing-target identity handoffs and the blocked FileLink/FileCopy mixed-effect handoffs.
They assert typed `directory_link` definition and desired hashes, the complete old and final typed effects where applicable, deterministic graph-schedule selection, and no executable action for every conflict.

Version-boundary tests use disposable profile, state, target, and store paths.
They prove that older profiles and state files, unknown future versions, unknown fields, invalid effect shapes, and `directory/copy` stop before target observation, recovery, or state rewrite.
They prove that a legacy state file with a matching target or active operation remains unchanged, is neither recovered nor adopted, and prevents a fresh plan.
State-decoder tests prove exact schema-6 `directory_link`, operation-graph, topology, and canonical-hash decoding, while rejecting every non-v6 state before it can enter recovery.
They table-drive every DirectoryLink operation-record shape, including ordinary and superseding create, remove, forget, relocate, state-only handoff, source-changing handoff, and replacement, and reject every missing, extra, or forbidden field before recovery.
Replacement fixtures prove the nested temporary predicate and compound postcondition have the exact recorded target paths and link targets, including the required equality between the temporary record and its missing postcondition and between the temporary link target and final effect link target.

## Filesystem Contract Tests

Filesystem contract tests run in a fresh temporary home, state directory, configuration directory, and local store.
They must never use the developer's real home directory, XDG directories, AppData directories, or a repository-owned state directory.

Each mutation test records the filesystem state before and after execution.
For a successful file-link operation, it asserts the final entry kind and normalized link target.
For rejection before any mutation step, it asserts that the target, its parents, the store and control files are unchanged. For a failed recheck after earlier steps or recovery, it asserts no effect from the rejected step and verifies the recorded aftermath of earlier effects separately.

The required negative cases include:

- a target regular file;
- a target link to a different source;
- a link that matches the desired source but lacks Known state;
- a missing, non-directory, symlinked, junction, or reparse-point parent;
- a source path that escapes or traverses an unexpected entry beneath the store root;
- a target outside the home root or inside a store or control path; and
- a replacement or removal whose actual link no longer matches Known state.

DirectoryLink filesystem tests additionally prove that the final source is a real directory without traversing its children, that an unmanaged matching directory link, file link, junction/reparse point, regular directory/file, and unsafe parent are all preserved conflicts, and that removal preserves the link referent and parent even when the former source has disappeared or changed.
For every DirectoryLink mutation path they snapshot disposable target, state, store, and control trees for validation, conflict, preflight, and dry-run rejection.

Tests that introduce changes observable at the immediate recheck must prove rejection before that mutation step without replanning. If an earlier action or temporary/relocation creation already ran, assert its recorded aftermath rather than zero mutation for the entire apply.

Tests must separately inject changes after the final recheck to characterize the [concurrency contract](../specs/file-link.md#external-filesystem-concurrency). Cover target and temporary substitution, source changes, and parent/ancestor relocation before recheck, after recheck and before post-observation. Do not assert atomic protection for a substituted entry or use test-only knowledge to force a race diagnostic. Prove that raced removal with all required postconditions, including `missing`, can be classified `succeeded`, remove Known and ultimately exit 0. If required recorded facts or declared-path association cannot be established, assert uncertainty; a handle-local postcondition alone cannot prove success for a different path.

File-link create integration tests must install an external matching link after the final recheck and before the no-replace syscall. Assert the already-existing-entry error, matching post-observation, preserved link/source, unchanged Known and retained `uncertain` operation. A matching observation after a failed create must not be asserted as success even when a test injects the error after a real create effect.
File-link create recovery tests must cover both an externally created matching link after the final recheck and a real create followed by interruption before the Known-state commit. In both cases recovery retains `uncertain`, preserves the matching link and empty Known, and blocks fresh planning. A missing create target closes the action as `failed`; wrong, regular-file, unsafe and unavailable observations remain `uncertain`.

Copy create tests must install an external matching regular file after the final recheck and before `create-no-replace`. Assert the collision, unchanged external bytes, unchanged Known, and retained `uncertain` operation. A matching observation after a failed create must not be asserted as success even when a test injects the error after a real create effect.
Copy create recovery tests must cover both an externally created matching file after the final recheck and a real create followed by interruption before the Known-state commit. In both cases recovery retains `uncertain`, preserves the matching file and empty Known, and blocks every fresh plan. A missing create target closes the action as `failed`; wrong, incomplete, unsafe, and unavailable observations remain `uncertain`. When direct create is enabled, tests must cover write, flush, close, and verification failures that leave a final-path artifact and prove that it is not automatically deleted, overwritten, adopted, or retried.

Copy replacement, copy `replace_ownership` with changed bytes, and `link -> copy` tests require separate target, temporary, source and parent rechecks after temporary creation and immediately before old-effect removal. They prove that `temporary_staged` is durably recorded after temporary verification and before old-effect removal, and that cleanup rejects a temporary whose durable `temporary_staged` fact is absent. They prove success only for exact new bytes after recorded publication, definite failure for the exact old owned effect or a missing final target after eligible-temporary cleanup, and failed conflict for a safely observed different final entry. Unsafe or unavailable observations remain `uncertain`. They assert that removal happens only after verified staging, that final publication does not overwrite an entry, and that no old-copy or old-link restoration occurs after publication failure. `copy -> link` tests prove the same old-effect recheck and remove-then-no-replace-create lifecycle using the link destination semantics without allocating a link-replacement temporary. Required cleanup tests cover an already-missing temporary; a removal error followed by proven missing; a denied removal; a reported-success removal whose missing aftermath cannot be proven; a remaining or different entry; an unsafe parent; and an unavailable observation. Only the first two complete cleanup and permit the recorded `failed` result; every other case retains `uncertain`, the active-operation barrier, and Known state until manual correction permits recovery. Preserve the stronger old-effect preservation assertions only for ordinary file-link replacement and source-changing link ownership replacement.

Windows path/state tests table-drive accepted normal DOS and ordinary UNC forms and rejected drive-relative, verbatim, device, reserved-name, trailing-space/period, dot-component, alternate-data-stream, and invalid-character forms. They prove separator canonicalization but case-exact collision, ownership, and hash behavior. They reject unsupported state versions before target observation or state rewrite; no test may accept them through implicit migration or alias comparison.

Managed identity-handoff tests cover link and copy effects.
For equal resolved link targets and same-fingerprint copy effects, they prove that the target is untouched, no replacement temporary path is allocated, and only the Known identity changes.
For source-changing links, they prove the Replace guarantees, including preservation of the old managed link when replacement itself fails, subject to the published external-concurrency limit.
For changed-fingerprint copy effects, they prove the staged copy replacement lifecycle and that verified success atomically replaces the old resource identity with the new one.
For a missing-target copy identity handoff, they prove that `create_copy` records the stale identity as superseded and that only verified create success replaces its Known record; failed or uncertain create leaves the stale record unchanged.

## State Repository Durability Tests

State tests use controlled failures at each commit boundary: temporary-file creation, write, flush, parse, validation, replacement, and directory flush when available.
They assert that a failed commit leaves either the prior valid state or a recoverable active operation record; it must never leave a partial authoritative state.

Recovery tests construct an active operation record and real filesystem observations for each case:

| Recorded action result | Expected recovery |
| --- | --- |
| The recorded post-condition holds | Commit the matching Known-state update and mark the action succeeded. |
| The recorded precondition still holds | Mark the action failed without changing prior Known state. |
| A pending action was never started | Mark it skipped without changing Known state. |
| Neither condition can be proven, or observation is unsafe | Retain the operation as uncertain and globally block every fresh plan and new planned target mutation. |

Copy recovery tests additionally cover a running sequential `replace_copy`, changed-fingerprint copy `replace_ownership`, `link -> copy`, and `copy -> link` action whose final postcondition holds with durable `publication_attempted`; recovery commits the new Known effect. For copy `replace_ownership`, that update removes the old identity and records the final copy under the new one. The same final observation without that success evidence remains `uncertain` and does not update Known. They cover an old precondition, a missing final target, and a safely observed different final entry; the first two close as failed only after required eligible-temporary cleanup is proven missing where applicable, while the different entry closes as failed conflict without target mutation. For `link -> copy`, different, ineligible, unsafe, or unavailable temporary observations during required cleanup retain `uncertain` and the active-operation barrier. They retain `uncertain` for unsafe or unavailable final observations and for incomplete required cleanup. A two-target `relocate_copy` partial state remains `uncertain`.

Relocation recovery tests inject an interruption after the new link is verified and before the old link is removed.
They assert that recovery retains that partial relocation as `uncertain` without target mutation.

DirectoryLink state tests cover `running` persistence before every mutation, the atomic verified Known update, and recovery from pending, expected old predicate, verified final predicate, and unsafe or unprovable observation.
Replacement and source-changing DirectoryLink identity-handoff tests cover the exact recorded temporary sibling after creation, before replacement, and during cleanup; only the recorded expected temporary may be removed, while a different, unsafe, unavailable, unremovable, or not-proven-missing temporary retains `uncertain` and the active-operation barrier.
They prove that the durable record precedes temporary creation, that replacement rechecks source, parent, target, temporary, containment, and declared-path association, and that an atomic-replacement error is classified from the final and temporary observations rather than its return value.
They separately prove final-postcondition success, old-precondition failure only after temporary cleanup, and preservation of the old Known identity when replacement or source-changing ownership handoff is uncertain.
DirectoryLink recovery tests construct each complete action record and prove its action-specific Known update, while a malformed record or a forbidden extra field is rejected before any target observation.

State-only identity-handoff recovery tests interrupt a `running` action before its atomic state commit.
They assert that the old Known identity and shared expected link or same-fingerprint copy atomically recover to the new identity and `succeeded` without target mutation.

Lock tests require two independently created repository handles or processes.
They must prove that the second non-dry-run apply fails before target observation or mutation while the first holds the exclusive lock.

## Executor and CLI Tests

Executor integration tests exercise the complete sequence from resolved inputs through state commit.
They inject a filesystem or state failure after a mutation where necessary and assert the resulting operation record and target state.
They prove that no other action begins between a `relocate_link` action's verified new-link creation and verified old-link removal.

CLI acceptance tests invoke the compiled binary in an isolated environment.
They assert behavior rather than exact prose formatting.
For example, they check that a blocked plan identifies a conflict and exits with status `2`, not the precise English wording of that diagnostic.
Apply confirmation tests prove that prompting follows successful preflight and that declined or unavailable confirmation creates no new operation record or planned target mutation. Include prior-operation recovery effects separately.
Schema-boundary acceptance tests prove status `2` for profile/state version mismatch and snapshot the target tree, state file, store, configuration, lock, and operation paths to show that no target inspection, recovery, conversion, or rewrite occurred.
They distinguish that status from status `1` for malformed JSON, unknown state fields, or violated state invariants.

`diff` acceptance tests construct Known state and expected, missing, wrong-link, other-entry, unsafe-parent, and unfinished-operation observations.
They assert that the command reports each category while leaving the target tree, state directory, store, configuration files, and operation record unchanged.
They also prove that `diff` neither needs nor reads a portable environment configuration.
DirectoryLink `diff` and `status` cases prove no-follow `expected_directory_link`, missing, other-link, other-entry, unsafe-path, and unavailable observations without following a directory-link referent.

Inspection acceptance tests invoke every inspection command with disposable home, state, configuration, and store directories.
They assert command selection and output categories rather than exact human-readable prose.
They snapshot the complete target tree, state directory, store, runtime configuration, portable configuration, and control files before and after each command; every snapshot must be identical.
They also prove that no inspection command acquires the exclusive lock, creates a lock or state directory, writes an operation record, or invokes recovery.

`profile list` and `profile show` tests prove that no source, target, or state access is necessary.
Desired-resource tests prove canonical resolution and no target/state access.
Known-resource tests prove that no declaration, source, or target access occurs and that an active operation is neither reported nor used to make a lifecycle decision.
`status` tests cover equal and different Desired/Known definitions, Desired-only and Known-only identities, no-follow observations for the union of relevant targets, active-operation reporting, declaration failure after a readable state, and the requirement that an invalid state prevents target observation.
An unsafe target/parent observation is asserted as reportable output; an unestablishable observation is asserted as status `1` with the documented partial report.

Dry-run acceptance tests compare snapshots of the target tree, state directory, store, and control files before and after the command.
The snapshots must be identical.

## Platform Conformance

Platform-neutral tests may use a filesystem abstraction for deterministic failure injection, but they do not replace real platform evidence.

The current binding and primitive choices are recorded in [File-Copy Platform Primitive Decisions](copy-platform-decision-record.md).
Before enabling a platform/action, native evidence must compare every chosen `create-no-replace` approach against its required collision and failure aftermath; a candidate that cannot establish the required post-mutation classification remains fail-closed.

The [intended supported scope](../specs/file-link.md#intended-supported-scope) requires the full action set on real Linux/ext4, macOS/APFS, and Windows/NTFS runners. Record OS/version, filesystem, Rust target/toolchain, capability, tests/results, and unrun cases. Native filesystem, executor/application, compiled-binary, and applicable recovery success evidence is required for every enabled capability, subject to the [Unix create evidence exception](../specs/file-link.md#unix-create-evidence-exception). That exception does not waive current create execution checks or native evidence on every baseline combination. macOS copy evidence includes the retained-parent APFS filesystem check and `VOL_CAP_INT_RENAME_EXCL` query before every enabled action, distinguishing enabled results from capability rejection. Where a capability is unavailable, prove safe preflight rejection instead of skipping the case.

Unix coverage must exercise no-follow final-link inspection, symlinked-parent rejection, atomic same-filesystem replacement and successful name-based removal under the observational concurrency contract, including the separate before/after-recheck cases above. Verify referents and parents are preserved in ordinary success and observed rejection cases. No atomic final-entry identity guarantee is required.
Windows coverage must exercise file symbolic-link behavior when available and reject junctions or unsupported reparse points.
It must also cover a replacement or removal rejected by access control or sharing when the test environment can create that condition, proving no final-entry overwrite, premature Known-state update, backup, or automatic restoration.
It must prove that a same-source managed identity handoff does not require replacement capability, while a source-changing handoff does require the documented replacement guarantee.
DirectoryLink native evidence separately proves directory-symbolic-link create, no-follow classification, replacement, relocation, and link-entry-only removal on Linux/ext4, macOS/APFS, and Windows/NTFS.
Windows evidence distinguishes directory symbolic links from file symbolic links, junctions, and other reparse points; it records a preflight rejection when directory-link creation or replacement guarantees are unavailable rather than skipping that action.
When the host cannot create a file symbolic link, cannot provide the required replacement guarantee, or cannot provide the required rechecked no-follow removal and observation guarantees, the test must prove the documented preflight failure rather than silently skipping the behavior.
Ordinary file-link replacement and source-changing link ownership replacement tests must cover interruption or failure after the action-local temporary link is created, proving that only the exact recorded temporary link may be cleaned up and that an unexpected or unremovable temporary entry leaves the action uncertain. Copy replacement and effect handoff requirements are defined above in the File Copies evidence section.

Windows capability evidence requires settled path/state semantics, retained-parent operation evidence, and target sharing/ACL denial aftermath separately from state-file sharing failures when the runner can establish those conditions. Symbolic-link policy or privilege denial is an action-time condition and must be classified from its post-observation; it is not a permanent preflight capability probe. Record conditions that could not be established as unverified.

Platform-specific tests run only in disposable directories and must clean up only the directories they created.

## Profile Parameters and Migration Evidence

Schema-5 declaration and resolver tests cover every closed parameter type, empty and non-empty scalar lists, whole-value references including `source.path`, root-parameter rejection, typed binding mismatch, unknown/missing/duplicate bindings, scope, invalid YAML lookalikes, nested-list rejection, and post-substitution resource validation.
They cover instance aliases, repeated equal-binding deduplication, conflicting repeated aliases, definition-reachability cycles, target collisions, and mapping/list-order and scalar-type canonicalization.
Hash fixtures prove schema-5 fully qualified IDs are lexicographically ordered in `loadout.resolved-desired.v5`, definition hashes exclude instance identity and raw parameter syntax, and alias changes identity while binding/effect changes do not.

Migration tests use disposable configuration, state, store, target, and temporary-publication trees and snapshot every target, store, profile, state, and relevant control-path entry before and after each case.
They prove normal lifecycle, validation, dry run, and inspection reject schema 3, 4, 5, and unknown documents before observation, recovery, planning, rewrite, or target mutation.

They also prove the migration-module boundary with a falsifiable static isolation oracle in `tests/migration_facade_isolation.rs`.
Before structural analysis, the oracle uses Cargo metadata to select the package and production crate roots, then constructs exactly three finite configuration records from the corresponding `rustc --target <triple> --print cfg` output: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, and `x86_64-pc-windows-msvc`.
It records each exact target triple, compiler configuration, target/root identity, active `cfg` or `cfg_attr` decision, and conventional external module edge, and rejects an unmodelled target/root, host or ambient cfg fact, unsupported predicate/source-loading form, inactive or orphan reachable source, missing or ambiguous child, duplicate/cyclic edge, `#[path]` redirection, generated source, or metadata disagreement before structural analysis.
The Linux and macOS records must select `src/executor/mod.rs` `directory_link` to `src/executor/directory_link.rs`.
The Windows record must select `directory_link_windows` to `src/executor/directory_link_windows.rs` and its cfg-gated crate-internal `directory_link_windows as directory_link` re-export, while leaving the Unix declaration inactive and unreachable.
Named inventory controls prove missing, extra, inactive, and path-redirected Unix and Windows declarations or re-exports fail in the applicable configuration before Stage 2.
Only after all three inventories succeed does Stage 2 parse the recorded active production items with `syn`, excluding `src/migration/**` from the non-migration structural scan.
Outside the migration module, the only permitted migration names are `MigrationFacade`, `MigrationRequest`, `MigrationPresentation`, and `MigrationOutcome` from the crate-root migration facade; aliases, globs, re-exports, wrappers, and every other migration item are rejected.
It rejects a legacy DTO/parser/converter import or call, including aliases, globs, re-exports, qualified descendants, and aliased YAML or JSON parsing.
For the dispatch path reachable from the sole public `loadout migrate` command, it rejects filesystem or inventory access, migration eligibility decisions, state-lock access, and temporary, flush, reopen, rename, replacement, or other publication capability outside the facade.
The oracle follows local crate calls and requires one presentation call and, after confirmation or `--yes` acceptance, exactly one direct facade execution call; it rejects unresolved or indirect dispatch and unexpanded/generated/macro bodies unless their exact bodies and finite callees are available for the same scan.
It names the source file, AST path or call, resolved import origin, and violated rule on failure.
Oracle fixtures include direct and aliased legacy parsing/conversion, inventory/preflight helpers, state-first and active-operation checks, state locks, publication operations, widened migration imports, function-pointer or closure dispatch, trait/generic or table dispatch, and included, generated, or macro-produced dispatch.
Each negative fixture must fail with its named rule or the indirect-dispatch rule; one positive fixture contains only request construction, one facade presentation call, confirmation, one facade execution call, and presentation/outcome rendering.
Separate route-boundary and facade-registration tests prove that private `schema_3_to_4`, `schema_4_to_5`, and `schema_5_to_6` routes are registered for both document kinds, that they are selectable only through the facade, and that a missing, duplicate, descending, or unregistered edge fails without reaching route parsing or publication.
The route-boundary oracle rejects a route-private import, re-export, or facade bypass outside `src/migration/`.
Add and retirement fixtures prove that adding a generation requires distinct state and profile routes plus explicit registration, while retiring one removes both and leaves normal readers schema-6-only.
This static oracle complements rather than replaces migration integration and CLI acceptance evidence.

Route tests cover 3→4, 4→5, their 3→5 composition, target 4, and complete validation of every in-memory intermediate and final candidate.
State fixtures cover the schema-3 default-instance identity condition, exact ID/effect/hash/ownership preservation, schema-4-to-5 empty-topology initialization, active or uncertain operation rejection, and no recovery.
Profile fixtures cover root-version-only conversion, full 5→4, 4→3, and 5→3 reverse compatibility proofs followed by exact forward byte reproduction, incompatible schema-4 parameters and schema-5 stages, and duplicate IDs, missing references, cycles, and target-inventory validation.
A matrix covers absent/3/4/5 state against mixed 3/4/5 profiles for target 5, and all no-newer-than-4 combinations for target 4; it rejects downgrade, unsupported, malformed, and unrepresentable cases before any publication.

CLI acceptance covers default target 6, explicit targets 4, 5, and 6, no-change success, confirmation decline, non-interactive `--yes`, every document and route in presentation, older-target normal-reader warning, and old subcommand argument errors that show the new syntax without writes.
It verifies one facade presentation call, no execution after declined confirmation, and one execution after confirmation or `--yes` acceptance, plus distinct internal state and profile responsibilities with state publication before profiles.
The confirmation snapshot tests change each selected configuration path or byte sequence, discovery directory path or safe association, complete entry set, profile path/kind/bytes, and state absence/bytes after presentation.
Run those changes with state absent, already at target, and requiring migration; none may publish an unconfirmed document.
Repeat a complete comparison before the first lock or publication, under a state lock when acquired, and before every profile publication; only successfully verified self-publications may update the expected snapshot.
Inject a change after state publication and after each profile publication, then prove the remaining confirmed write set is untouched and a fresh invocation can validate and resume.

Publication tests cover state lock contention, validated same-directory temporaries, replacement failure retaining the original when the platform promises it, no-follow profile-path and temporary rechecks, post-publication verification failure, and cleanup limited to a proven owned temporary.
They assert byte-for-byte unchanged documents on pre-publication rejection, classify an unprovable post-publication result as failure, and never claim rollback of earlier verified publications.
Every migration outcome proves zero target, store, materialization, and ownership mutation.
Native Unix and Windows evidence exercises same-directory replacement and state-lock contention in disposable directories and records operating-system version, filesystem, Rust toolchain, exact command, and whether tests ran natively; cross-compilation is not Windows-native evidence.
An unavailable required capability demonstrates the documented fail-closed preflight instead of being skipped.

## Schema-6 Orchestration Evidence

Pure resolver tests cover root-only phases, child phase participation, outer-over-inner `includes[].stage` priority, resource declaration fallback, syntactically invalid overridden names, effective missing phases, a child selected alone, and rejection of included child `stages`. They cover repeated equal-binding instances with equal and conflicting effective memberships, including equal memberships from an include assignment and a resource declaration; the displayed source must follow the stable alias-path tie-break and remain unchanged under include permutations. They cover local-only `depends_on`, qualified-reference rejection, parent-relative alias paths, distinct instances, out-of-closure and ambiguous paths, self and duplicate canonical edges, and deterministic output under mapping and include permutations. Hash fixtures prove that phase, dependency, and provenance edits do not change ownership hashes.

Planner tests cover branching and joining phase graphs, `A → B → C` with empty `B`, an unchanged recorded `B`, and a successfully acted-on `B`, direct prerequisites within a phase and across instances, satisfied Known predecessors, unstaged resources, role legality, combined cycles, and deterministic schedules. State and recovery tests prove that, with unchanged recorded B and successful A and C actions, C records both `A → C` and `B → C` while no new `A → B` appears. If B also has a successful output action, recorded `A → B → C` permits omission of `A → C`. Subsequent stale removal must reverse exactly the recorded relations, including A-before-C in the unchanged-B case. Tests also prove exact pre-recorded authority and deltas, success-only membership and relation publication, partial success, dependency and global-abort skips, stale removal from recorded topology only, rejection of empty durable barriers and malformed references before target observation, and unchanged state bytes for ordering-only edits. A newly declared phase never attaches an unchanged Known resource to durable topology.

Focused state tests cover both advance arrays with exact before and after facts: member addition, independently proved `after` addition, and `after` addition by a successful output action whose membership is already recorded. They reject an advance by a stale or unsuccessful action, unproved or missing predecessor, unrelated member addition, member or relation removal, duplicate barrier ID across delta positions, and an invalid resulting topology. Stale tests remove a final predecessor member and atomically remove every surviving successor `after` reference, including one with current members; they reject dangling references, unrelated reference removal, and cleanup not authorized by that stale action. Partial-success and recovery tests show that only successful action deltas publish, while failed or uncertain actions retain their recorded facts and exact authority without replanning or transferred ownership.

State decoding rejects an applied direct endpoint or barrier member absent from loaded Known before target observation, while an active operation's immutable stale snapshot may retain an ID removed by an earlier verified action in that operation.
Operation-record validation rejects a direct-edge addition whose predecessor is a recorded action but whose action graph lacks the matching dependency edge, and rejects deletion of an all-stale direct edge by any action other than its stale successor.
Loaded-operation tests reject a direct-edge addition with no predecessor action when its predecessor is absent from Known, and reject a succeeded successor when its recorded predecessor action failed. They accept a prior Known predecessor and an earlier succeeded predecessor action.

Planner and operation tests also cover an existing recorded `after` relation removed from current Desired while a new output member is added: unchanged Known predecessors satisfy it, current predecessor actions are scheduled and must succeed first, and stale or otherwise unprovable predecessors block before mutation. The recorded operation and recovery preserve that old relation without deriving it from changed declarations.
Planner and CLI rendering tests verify that ordering failures report their actual reason, including an unproved direct or recorded predecessor and an unproved topology publication, without reporting an unsupported `noop` action.

Recorded-order conflict tests compare resource reachability even when phase names change and no action is needed. They assert both directions and sources, conservative connected-component expansion through recorded direct edges and barriers, and zero new operation or target mutation on a blocked new relation. An end-to-end disposable CLI fixture removes the complete reported rebuild set from Desired, applies stale removal under old topology until every member and old fact is gone, restores the new declarations, then plans and applies the new order. Injections of failed and uncertain removal must stop the sequence until recorded-state recovery or retry succeeds; partial removal is never reported as a complete rebuild. Prior-operation recovery effects are asserted separately from rejection of the new plan.

Migration route tests cover 5→6 state with retained Known and both topology arrays empty; they reject an active operation or either nonempty array before publication, including 3/4→6 compositions. They cover old child phases and qualified `after` references that have a unique root meaning, conflicting same-name `after`, cross-instance dependency relocation to a unique parent, ambiguous aliases and multiple root contexts, uncovered profiles, invalid default roots, and invalid fully resolved candidate roots. A parameter-bound source path that becomes invalid after substitution rejects the complete inventory without publication; a valid bound path migrates and passes normal validation. A target inside a configured local store rejects before publication with the complete inventory unchanged; moving it to an allowed target permits migration and normal validation. The mixed-inventory matrix includes absent and 3/4/5/6 state against 3/4/5/6 profiles, reverse compatibility of a schema-6 profile against older state, and schema-6 state with recorded topology. Presentation tests identify actual broadening of old `after` constraints before confirmation and omit that claim when scope is unchanged. Snapshot, state-before-profile publication, interruption, resumption, and zero target/store mutation checks remain required.

CLI and inspection acceptance tests prove selected-root `validate`, default selection, `validate --all` status `2`, and document-only discovery distinct from selected-root resolution. Desired-resource queries and `plan` expose effective phase, winning include path or own declaration, normalized direct dependencies, and their declaration sources without reading Known or targets for Desired inspection. They snapshot disposable configuration, store, state, operation, and target paths to prove invalid declarations, read-only queries, and dry runs make no changes. Resource-action platform tests continue to require the native evidence described above; ordering-only logic introduces no new platform filesystem primitive.

## Change Checklist

Before a change is ready for review:

1. Link the changed behavior to its architecture or specification owner.
2. Add or update the required test-layer evidence from the contract matrix.
3. Include at least one negative test for every new mutation path.
4. Include a zero-mutation test for every new dry-run or validation path. For blocked Plans and preflight failures, prove no new planned target mutation or new operation record, and assert permitted prior-operation recovery effects separately.
5. Add platform evidence when a behavior depends on symbolic links, path normalization, locking, or replacement semantics.
6. Record validation commands that could not run; do not claim unrun checks passed.
