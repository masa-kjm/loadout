---
name: maintain-loadout-context
description: Maintain Loadout v0.2 documentation and project skills without changing their authority boundaries. Use when updating docs, navigation, agent instructions, or source skills and their generated discovery copies.
---

# Maintain Loadout Context

Keep documentation and project skills accurate, discoverable, and derived from the authoritative v0.2 contracts. Do not use this skill to choose an unresolved product behavior or to implement a code-only change with no documentation or skill impact.

## Identify the maintenance surface

1. Read `docs/README.md`, the target, and the documents that own the relevant behavior or architecture before editing.
2. Classify the change as documentation, project-skill maintenance, or both. Read [documentation authority](references/documentation-authority.md) for documentation changes and [skill maintenance](references/skill-maintenance.md) for skill changes.
3. If a task would change a safety-, ownership-, persistence-, platform-, schema-, or compatibility-sensitive contract, find its authoritative architecture or specification owner. Report an unresolved conflict rather than deciding it through an example, implementation detail, `future/`, draft, `AGENTS.md`, or a skill.

## Update documentation

- Change the canonical document first. Update summaries, navigation, and examples only when they own an affected claim or route to it.
- Keep architecture responsibilities, observable specifications, testing strategy, and future designs in their separate documentation layers.
- Keep user-facing documentation in English. Use relative repository links and durable headings.
- Do not claim a command, schema, platform guarantee, release process, or installed capability that the authoritative documents and implementation do not support.

## Update project skills

- Treat `.claude/skills/` as source and `.agents/skills/` as generated Codex discovery output. Do not edit generated copies directly.
- Keep `AGENTS.md` and `CONTRIBUTING.md` as derived guidance. They may explain authority and workflow but must not redefine v0.2 behavior.
- Keep a skill narrow. It may route to authoritative documents, but it must not duplicate or silently alter their normative rules.
- When creating or materially restructuring a skill, use the active environment's skill-creation guidance when available. Keep only references and scripts that improve a real recurring workflow.
- Synchronize source skills with `scripts/sync-codex-skills.sh`. Do not use `--prune` unless removal of each managed generated skill is in scope.

## Validate and hand off

1. Read the changed page or skill as its intended user would, including its inbound navigation or invocation route.
2. For documentation links or headings, run:

   ```sh
   python3 <skill-dir>/scripts/check-local-links.py README.md docs
   ```

   The checker covers ordinary repository-local inline links and heading fragments. Inspect unsupported Markdown constructs manually.
3. For a changed source skill, run the available skill validator, synchronize its generated copy, and verify that source and generated content match except for the synchronizer marker.
4. Run `git diff --check`. Run focused code, CLI, or contract checks when a changed claim depends on executable behavior.

Do not claim unrun validation passed. In the handoff, name the canonical source, the dependent pages or generated skills updated, and any unresolved authority or validation limitation.
