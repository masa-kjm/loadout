# Test-layer Selection

The authoritative test requirements are [Testing Strategy](../../../../docs/development/testing.md). Read the applicable contract matrix row before finalizing a plan.

Select the smallest layer that can observe the required fact:

| Test layer | Use when the contract concerns |
| --- | --- |
| Pure domain | Typed resolution results, validation, Desired/Known/Actual classification, planning, or deterministic order without I/O. |
| Filesystem contract | No-follow observation, physical containment, entry kind, link mutation, or the absence of a forbidden mutation. |
| State repository durability | Locks, schema validation, operation progress, atomic commit, or recovery with a controlled commit failure. |
| Executor integration | The lifecycle across resolved inputs, real temporary files, the filesystem implementation, verification, and Known-state commit. |
| CLI acceptance | Arguments, confirmation, dry run, output category, or exit status in an isolated process. |
| Platform conformance | Unix or Windows behavior that a platform-neutral fake cannot prove. |

Pure tests must use resolved paths and typed observations only. They must not parse YAML, access a store, inspect the host filesystem, acquire a lock, or serialize state.

An observable CLI or filesystem contract needs integration or acceptance evidence in addition to a private unit test. A blocked-plan test must prove both the blocking diagnostic and the absence of an executable action for the conflicting target.

Keep fixtures only for intentional stable contracts. Do not assert exact human-readable diagnostic prose when the CLI specification defines an output category or exit status instead.
