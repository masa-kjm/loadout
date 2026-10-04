# Platform-conformance Test Design

The authoritative platform contracts are [File Links](../../../../docs/specs/file-link.md) and [File Copies](../../../../docs/specs/file-copy.md), with required evidence in [Testing Strategy](../../../../docs/development/testing.md).

Platform-neutral fakes may inject deterministic failures, but they cannot replace real platform evidence. Use the [supported scope](../../../../docs/specs/file-link.md#intended-supported-scope) for the Linux/ext4, macOS/APFS and Windows/NTFS completion baseline. Rejection alone does not complete successful action support. Keep current rejection tests until enablement, then add native success and actual capability-failure evidence.

Separate substitutions observable at the final recheck from races afterward under the [concurrency contract](../../../../docs/specs/file-link.md#external-filesystem-concurrency). Cover targets, recorded temporaries, sources and parent/path association; include indistinguishable successful removal without a test-only race diagnostic. Require uncertainty when recorded facts are unavailable, and do not infer declared-path success from a retained handle alone. Keep before-recheck rejection, temporary cleanup, partial-effect and recovery evidence.

For Unix, design real-filesystem coverage for no-follow final-link inspection, symlinked-parent rejection, atomic replacement of an expected managed link, and removal of the link entry without touching its referent. For sequential copy replacement and effect handoff, separately cover verified staging or final-link preparation, fresh old-effect removal, no-replace publication, and the documented failure aftermath.

For Windows, design real-filesystem coverage for file symbolic-link behavior when available, rejection of junctions and unsupported reparse points, and replacement or removal denied by access control or sharing. For ordinary link replacement, that denial must prove there is no delete-then-create fallback and no premature Known-state update. For sequential copy replacement and effect handoff, it must prove no final-entry overwrite, premature Known-state update, backup, or automatic restoration.

When the host cannot create a file symbolic link or cannot provide the replacement guarantee, the test must prove the documented preflight failure rather than silently skipping the behavior. Tests must use only disposable directories and remove only directories they created.
