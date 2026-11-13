# File-Copy Platform Primitive Decisions

This page records the current implementation choices for file-copy publication. It does not define runtime behavior. [File Copies](../specs/file-copy.md) owns the observable contract, and [Testing Strategy](testing.md#platform-conformance) owns native evidence and capability validation.

## Binding policy

Use narrow bindings that preserve the retained-parent and no-follow guarantees:

- `rustix` for Unix retained-parent filesystem operations;
- `libc` only for Darwin capability queries or constants unavailable through `rustix`; and
- `windows-sys` for documented Win32 and NT calls.

Do not add a high-level copy or move library, a generic cross-platform filesystem abstraction, path re-resolution, backup/restore, or an untracked fallback. Copy replacement and effect handoff use only the specified verified staging, owned-old removal, and no-replace publication sequence. Review any new binding or direct FFI declaration for API fit and maintenance cost, then establish native success, collision, failure aftermath, and recovery evidence before enabling its action.

## Selected primitives

| Platform | Current choice |
| --- | --- |
| Linux/ext4 | Retain parent handles and use `rustix::fs::renameat_with` with `RenameFlags::NOREPLACE` for staged copy publication. Use retained-parent `rustix` remove and rename operations for replacement, relocation, removal, and link/copy handoff. |
| macOS/APFS | Use retained-parent `rustix` operations for staged no-replace publication and removal. Query the retained parent for APFS and `VOL_CAP_INT_RENAME_EXCL` through narrow `libc` bindings before each enabled copy action; reject the action at preflight when the required capability cannot be established. |
| Windows/NTFS | Use retained-parent `windows-sys` operations. Direct exclusive creation uses `NtCreateFile(FILE_CREATE)`; staged no-replace publication uses handle-relative `NtSetInformationFile(FileRenameInformation)`. Replacement and handoff use verified staging, owned-old removal, and no-replace publication. `ReplaceFileW` is excluded because it does not implement this sequence. |

Changed-fingerprint copy ownership handoff reuses the replacement primitive and commits the old-to-new Known identity update atomically. A same-effect identity handoff needs no additional filesystem primitive.

The baseline combinations have enabled copy actions. Other platform/filesystem combinations remain subject to the capability gates in [File Copies](../specs/file-copy.md) and [File Links](../specs/file-link.md#intended-supported-scope). Native evidence must continue to cover the selected primitive's collision preservation, post-error classification, executor, compiled CLI, and recovery behavior as described in [Testing Strategy](testing.md#platform-conformance).
