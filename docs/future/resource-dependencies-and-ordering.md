# Resource Dependencies and Ordering

## Status

This is a non-binding design note.
v0.2.0 has no user-configurable per-resource execution order.
It uses lifecycle phases followed by fully qualified resource-ID order only as a deterministic tie-breaker.

## Why Declaration Order Is Not an Ordering Contract

Profile declarations are composed through includes, discovered from multiple files, and represented partly by mappings.
Using their textual position as execution semantics would make behavior depend on include traversal, file refactoring, serializer behavior, and implementation collection choices.
It would also make an otherwise harmless reordering of declarations change side effects without making the required relationship visible in the resource model.

Fully qualified resource IDs are likewise identities, not order numbers.
Renaming an ID to influence lexicographic execution would change durable resource identity and can turn a definition update into a replacement or removal decision.
Neither declaration position nor ID spelling is an acceptable user-facing ordering mechanism.

## Candidate Model

A future schema may add explicit dependency references such as `depends_on` to a resource declaration or resource-type-specific properties.
The final syntax is undecided, but every reference must resolve to a stable resource or resource-instance identity after profile composition and parameter binding.
Stages may provide coarse execution barriers, but they are a distinct ordering-only concept described in [Stages and Execution Ordering](stages-and-execution-ordering.md).

The resolver and planner would construct an action graph rather than relying only on phase-local sorting:

1. validate dependency references and reject cycles;
2. derive resource actions through the normal Desired/Known/Actual rules;
3. add only semantically valid dependency edges between planned actions;
4. reject an edge that conflicts with a required ownership or safety transition; and
5. execute a deterministic topological order, using the canonical fully qualified resource-ID order only among otherwise independent ready actions.

A dependency means more than preference: a dependent action may start only after the required predecessor reaches the post-condition defined by its resource contract.
The executor still receives a fixed Plan and must not independently discover, reorder, or skip dependencies.

## Recommended Evolution Path

`depends_on` should be introduced as a resource-level declaration whose references resolve to canonical resource-instance identities.
It must not refer directly to an include, profile, stack, or generic group.
Those concepts compose or select desired state; they are not ownership, execution, state, or recovery units.
If a future profile- or stack-level convenience syntax is justified, the resolver must expand it into explicit resource-instance dependencies before planning.

A stage is not a generic group or a resource dependency target.
It may expand a stage-to-stage barrier into action edges, while direct `depends_on` remains the mechanism for an exceptional resource-level prerequisite.

The first shipped syntax should support only hard dependencies, with an empty dependency set as its default.
It should require fully qualified, stable references rather than context-relative names, numeric priorities, declaration positions, or arbitrary `before`/`after` hints.
This keeps a reference stable when a profile is refactored and makes every required ordering relationship visible in the resolved graph.

The existing v0.2.0 schemas reject unknown fields, which is intentional: an older binary must not silently ignore an ordering requirement.
When dependency behavior is shipped, it should use a new profile schema version rather than making a non-empty `depends_on` field an undocumented extension of version 1.
The newer implementation may read version-1 profiles only to migrate them; lifecycle commands must require explicit migration before operating on a schema version other than the version they implement.
Migration must preserve resource identity and effect semantics when possible, and must report an explicit user decision when it cannot; see [Schema Evolution and Migration](schema-evolution-and-migration.md).

The dependency set is orchestration data, not part of a resource's materialized filesystem effect or ownership proof.
Changing only `depends_on` must not by itself plan a create, replacement, relocation, or removal for that resource.
It may change the Plan's action graph and the outcome after a predecessor fails, but it must not change resource identity or authorize a target mutation.
The design should therefore keep an effect/ownership fingerprint separate from a desired-set or operation fingerprint that includes dependency edges.

State migration requires `active_operation == null`.
If an old operation is active or uncertain, migration must reject; the binary implementing the original state contract must recover and close that operation before the explicit state-migration protocol can run.
The dependency-aware operation record must store the resolved action edges and the reason an unattempted dependent action was skipped, so recovery never has to rediscover ordering from changed declarations.
For stale-resource removal ordering, the durable representation is a compact Applied Lifecycle Topology snapshot rather than source-level stage declarations or a fully expanded action graph; see [Stages and Execution Ordering](stages-and-execution-ordering.md).

## Lifecycle Constraints

The existing create, replacement, and removal safety phases cannot be bypassed by an ordering request.
For example, a request cannot cause an unmanaged target to be removed earlier, nor can it turn a blocked predecessor into an executable action.
If an action fails, its dependents must be skipped or left unattempted with a recorded reason; already verified independent actions remain governed by the normal no-rollback rule.

Dependency semantics must also define stale-resource removal.
Applied Lifecycle Topology retains compact historical stage-barrier and direct-resource relationships, and may supply reverse ordering for safe stale removals without becoming ownership evidence.
The design must define how a changed dependency graph is represented in a plan and in the compact topology snapshot.
Partial apply, if ever added, must include the complete dependency closure or be rejected.

## Alternatives Not Chosen

An integer priority, arbitrary `before`/`after` hints, and declaration-order semantics are attractive initially but make it difficult to tell whether a relationship is required for correctness or merely convenient.
They also create fragile behavior when profiles are composed or resources are renamed.
If a future need is genuinely a non-semantic preference, it should have a separate, explicitly weaker contract and must not override dependency, safety, or ownership rules.

A generic group is not a prerequisite for dependency support.
Groups tend to conflate reporting, selection, ownership, and execution order; none should gain dependency semantics implicitly.
They can be added later as a presentation or selection feature, or as explicit syntactic sugar that expands to resource-instance edges under the same rules.
Stages are deliberately narrower than such groups and are specified separately.

## Required Promotion Work

Promotion requires a final schema and versioning plan, stable instance-reference rules, effect-versus-orchestration fingerprint rules, cycle and missing-reference diagnostics, action-graph and stale-removal semantics, Applied Lifecycle Topology representation, operation-record and recovery representation, and tests for version-1 compatibility, composed profiles, parameterized instances, independent tie-breaking, cycles, failed predecessors, dependency-only edits, topology updates after partial failure, state-upgrade interruption, changed dependencies, partial-apply closure, and platform-specific resource effects.
