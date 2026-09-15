# v0.2 Scenario Set

These scenarios are the initial contract for an automated CLI acceptance runner. Each scenario must create fresh temporary home, configuration, state, and local-store directories, copy the shared fixture inputs, and remove only directories it created.

| Scenario | Expected evidence |
| --- | --- |
| `validate` | A valid environment and profile pass validation without inspecting or mutating the target. |
| `plan` | An empty Known state and missing target produce an executable create plan without mutation. |
| `apply` | The file link is created, verified, and recorded in Known state. |
| `apply-again` | Repeating apply produces only `noop` behavior and leaves target, state, and store content unchanged. |
| `diff` | Managed, missing, wrong-link, and other-entry observations are reported without mutation. |
| `dry-run` | Apply dry-run leaves target, state, store, and control files byte-for-byte unchanged. |
| `blocked-unmanaged-target` | An unmanaged target blocks the plan and is not removed, replaced, or adopted. |
| `blocked-unsafe-parent` | An unsafe target parent blocks the plan without a new operation record or target mutation. |

The runner should use `--yes` for non-interactive apply and assert exit-status classes from the [CLI specification](../../../docs/specs/cli.md). It should assert behavior and filesystem/state snapshots rather than exact human-readable wording.

Migration scenarios belong here only after a migration contract is published. Their input and expected state documents should then be added under [shared migration fixtures](../../fixtures/README.md).
