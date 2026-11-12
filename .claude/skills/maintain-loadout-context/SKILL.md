---
name: maintain-loadout-context
description: Maintain Loadout documentation, agent instructions, and project skills while preserving document authority. Use for navigation, documentation, or skill changes.
---

# Maintain Loadout Context

Keep guidance derived from the current authoritative contracts. Do not decide unresolved product behavior through a skill or summary.

## Identify the maintenance surface

1. Read `docs/README.md`, the target, and its owning documents. Read [documentation authority](references/documentation-authority.md) for document changes or [skill maintenance](references/skill-maintenance.md) for skill changes.
2. Report an unresolved safety-, ownership-, persistence-, platform-, schema-, or compatibility-sensitive conflict in the authoritative documents instead of choosing behavior from derived guidance.

## Update documentation

- Change the canonical document first, then affected summaries and navigation. Keep user-facing documentation in English with relative links and durable headings.
- Check claims about commands, schemas, platforms, releases, and installed capabilities against the owning documents and implementation.

## Update project skills

- Edit `.claude/skills/`, not generated `.agents/skills/` copies; synchronize after validation. Keep `AGENTS.md`, `CONTRIBUTING.md`, and skills derived from the authoritative documents.
- Keep each skill narrow and route to contracts instead of restating them. Use the environment's skill-creation guidance for a material restructure.

## Validate and hand off

Read the result through its navigation or invocation route. Check local links with `python3 <skill-dir>/scripts/check-local-links.py README.md docs`, validate changed skills, synchronize generated copies, compare them with their sources, and run `git diff --check`. Run focused executable checks when a changed claim depends on behavior. Report any unrun check or authority limitation.
