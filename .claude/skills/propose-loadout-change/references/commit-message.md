# Commit Messages

Use the actual behavior in the diff to choose one primary Conventional Commit type.

| Change | Type |
| --- | --- |
| Fixes incorrect existing behavior | `fix` |
| Adds user-visible behavior | `feat` |
| Changes documentation only | `docs` |
| Adds or corrects tests only | `test` |
| Restructures without behavior change | `refactor` |
| Changes CI or workflow automation | `ci` |
| Performs maintenance or release metadata work | `chore` |

Use this subject format:

```text
<type>(<scope>): <imperative summary>
```

The scope is optional.
When it adds clarity, choose the narrowest stable responsibility or behavior area:

1. User-visible operation: `config`, `profile`, `file-link`, or `cli`.
2. Internal responsibility: `resolve`, `planner`, `executor`, `state`, or `fs`.
3. Cross-cutting work: `docs`, `ci`, `deps`, or `release`.

Omit the scope when the change spans multiple areas or no single area is clearly primary.
Do not use an issue number, filename, or generic scope such as `misc`.

Write the subject in English, imperative voice, without a trailing period.
Describe the outcome rather than an implementation detail, and keep it short enough to scan in Git history.

For example:

```text
feat(file-link): create verified managed links
fix(planner): reject unmanaged matching links
refactor(state): isolate operation-record commits
docs: clarify uncertain recovery
```

Use `Refs: #N` in an optional commit body when it improves traceability.
Do not use `Closes #N` in a speculative local commit proposal.
For multiple issues, choose the issue that owns the behavior as the primary reference and list others only when the diff genuinely addresses them.
