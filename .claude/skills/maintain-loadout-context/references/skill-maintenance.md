# Project-skill Maintenance

Project skill sources are direct children of `.claude/skills/`. Each source skill requires `SKILL.md` and may contain `agents/`, `references/`, or `scripts/` only when they support its actual workflow.

`.agents/skills/` is a generated and ignored Codex discovery directory. Use `scripts/sync-codex-skills.sh --dry-run` to preview synchronization, then run the synchronizer normally. It marks managed copies and refuses to replace an unmanaged target. `--prune` removes stale managed copies and is appropriate only when those removals are explicitly in scope.

Keep `SKILL.md` concise and discriminating. The description determines when a skill is selected; instructions should provide only Loadout-specific decisions, authority routing, safety boundaries, and necessary validation. Put conditional procedures in a linked reference instead of duplicating a specification.

Skills are derived guidance. They must not create a v0.2 contract, authorize GitHub or Git mutation, expand a task's scope, or override `docs/architecture/` or `docs/specs/`.

After source changes, validate each changed skill with the active environment's skill validator when available. Then compare source and generated copy while excluding `.loadout-codex-adapter`.
