# Documentation Authority and Navigation

The authority model is defined by [Documentation](../../../../docs/README.md). Read that page before changing a behavior claim.

| Location | Owns | Does not own |
| --- | --- | --- |
| `README.md` | Landing-page promise, project status, and entry links | Exhaustive command behavior or normative rules |
| `docs/README.md` | Documentation authority, v0.2 scope, and reading routes | A replacement for an architecture or specification contract |
| `docs/architecture/` | System model, responsibility boundaries, dependency direction, and safety boundaries | YAML schema, command syntax, state format, or filesystem algorithms |
| `docs/specs/` | Observable CLI, schema, lifecycle, ownership, path-safety, state, recovery, and failure-aftermath contracts | Tutorial prose or implementation layout |
| `docs/development/` | Review and testing practice that proves contracts | Runtime behavior or a new error outcome |
| `docs/future/` | Non-binding constraints and promotion considerations for later work | A v0.2 behavior change |

When a canonical document changes, update its direct navigation and dependent summary only when necessary:

- update `docs/README.md` when a documentation layer, page inventory, authority, or v0.2 scope changes;
- update the relevant directory README when its document inventory changes;
- update `README.md` only when the landing-page promise, status, or entry navigation changes; and
- update `AGENTS.md` or `CONTRIBUTING.md` only when their derived guidance becomes inaccurate.

Do not create a second specification in a README, guide, skill, or agent instruction. Prefer a concise summary plus a relative link to the canonical page.
