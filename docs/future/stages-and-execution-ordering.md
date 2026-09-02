# Stages and Execution Ordering

## Status

This is a non-binding design note.
Stages are not part of v0.2.0 and this document defines no stage schema, resource field, or CLI behavior.

## Role

A profile describes which resources belong to a composed desired state.
It is not an execution stage.
A stage is a deliberately narrow orchestration concept: it groups resources behind a named execution barrier and can depend on other stages.

Stages are not resources, ownership domains, state namespaces, profile overrides, or generic groups.
They have no filesystem effect of their own.
Direct resource dependencies remain the fine-grained mechanism for an exceptional prerequisite; see [Resource Dependencies and Ordering](resource-dependencies-and-ordering.md).

## Resolved Model

Future resolution should produce explicit stage data alongside resolved resources:

```text
Resolved Desired
  resources: ResolvedResource[]
  stages: ResolvedStage[]

ResolvedResource
  resource-instance identity
  effect definition
  optional resolved stage membership
  direct resource dependencies

ResolvedStage
  stable stage identity
  stage prerequisites
```

Source syntax may permit local names, but resolution must produce stable, composition-safe stage identities before planning.
Stage identity must not depend on YAML position, include traversal order, or implementation collection order.
The final design must define how reusable profiles and future parameterized profile instances refer to stages without accidental name capture.

A resource may omit stage membership.
An unassigned resource is not silently placed in a default barrier and keeps the ordinary deterministic ordering behavior.

## Orchestration-Only Metadata

Stage membership and stage dependencies are execution-order metadata only.
They MUST NOT affect resource ownership, resource identity, Actual-state classification, drift detection, or the materialized filesystem effect.

Changing only a resource's stage or only a stage dependency MUST NOT produce a create, replacement, relocation, removal, or other resource mutation.
It may change the order of actions when an apply already has resource work to perform, but an ordering-only edit with no resource work does not itself require an apply operation or a state rewrite.

The implementation should keep two distinct representations:

- an effect and ownership fingerprint, used for Known state and resource transition decisions, which excludes stage metadata and direct dependencies; and
- an orchestration fingerprint, used for a desired-set or operation record, which includes resolved stage and dependency edges.

Known state must not store source-level stage membership or treat it as evidence of ownership.
An operation record may retain the already-expanded action edges and skip reasons needed to report a partially executed operation truthfully, but recovery must use recorded action preconditions and post-conditions rather than reevaluating stage declarations.

## Applied Lifecycle Topology

Reverse ordering of stale-resource removals needs information that is no longer present after those resources leave Desired state.
To provide that ordering without making source-level stages durable resource properties, future state may store an **Applied Lifecycle Topology** snapshot.

The snapshot is derived after profile composition and contains a compact, source-independent representation of the last verified lifecycle relationships among Known resource IDs, including both expanded stage barriers and direct resource dependencies.
It records membership of opaque barrier units and their prerequisite edges, not source stage names, YAML paths, include positions, or a fully expanded action graph.
For example, a barrier between fifty predecessor and fifty dependent resources remains one topology edge rather than 2,500 durable action edges.

Applied Lifecycle Topology is orchestration history, not ownership evidence.
It cannot create a removal, replacement, or other mutation action; it can only order actions that the resource classifier has already authorized through the normal Desired/Known/Actual and ownership rules.
Renaming or reorganizing source stages therefore does not change resource identity or require a resource migration.

The snapshot must be atomically consistent with the Known resource set committed at that point.
After each verified resource result, the state repository commits the resource's Known-state change, operation status, and the corresponding topology snapshot update together.
Following a partial failure, it MUST NOT store the unapplied Desired topology as if every resource had succeeded; the stored topology must describe only the resource effects that are actually Known.

An ordering-only edit without resource work does not itself rewrite the snapshot.
The final promotion design must specify how a later successful operation incorporates current topology for still-desired resources while retaining applicable historical topology for stale resources.

## From Stages to an Action Graph

The planner remains pure.
It first classifies each resource from Resolved Desired, Known, and Actual state, then builds one action graph from:

1. lifecycle safety and replacement edges;
2. direct resource dependency edges; and
3. stage-barrier edges; and
4. applicable edges from Applied Lifecycle Topology for stale-resource removals.

A stage edge from `foundation` to `runtimes` is expanded into a barrier over relevant action results, not a textual ordering of resource declarations.
Every relevant predecessor action in `foundation` must reach the completion condition defined by its resource contract before a dependent action in `runtimes` may start.

A resource already proven to satisfy its required post-condition contributes no executable action to the barrier.
It is therefore already satisfied rather than requiring a synthetic mutation or an ordered `noop` action.
If a predecessor action fails, becomes uncertain, or is blocked, dependent actions are skipped or blocked with a recorded reason.

The planner MUST validate the combined action graph for missing references and cycles.
An acyclic stage graph and an acyclic resource-dependency graph can still form a cycle after expansion, so they are not sufficient checks independently.
The executor receives the resulting Plan and must not create, reorder, or infer graph edges.

Within the set of graph-ready independent actions, execution remains deterministic through the canonical fully qualified resource-ID tie-breaker.
Stages never authorize an action that fails ownership, path-safety, preflight, or other lifecycle checks.

## Removal Boundary

For current desired-resource actions, a stage edge orders applicable create, update, and replacement actions in its forward direction.
For two stale resources related in Applied Lifecycle Topology, the corresponding authorized removal actions use the reverse direction.
For example, an applied `package -> runtime` relationship produces `runtime.remove -> package.remove` when both resource removals are safe and planned.

The same action-pair conversion applies to a direct resource dependency retained in Applied Lifecycle Topology.
The topology representation must preserve whether an edge came from a barrier or a direct prerequisite when that distinction affects diagnostics or expansion, but neither kind is ownership evidence.

This is not a blind reversal of every stage edge.
The Action Graph Builder expands only the edges justified by the classified action pair, and replacement retains its own lifecycle preconditions and post-conditions.
No topology edge may bypass target ownership, path safety, preflight, or conflict checks.

Mixed current-desired and stale-resource cases require an explicit action-pair rule.
The final specification must define every supported pair rather than inferring an edge from source stage names or treating a lifecycle topology snapshot as an executable Plan.

## Schema and Promotion Constraints

Stage support should be introduced only with a new profile schema version so older binaries cannot ignore ordering requirements.
Lifecycle commands must require explicit migration of older profile and state schemas before operating on them; see [Schema Evolution and Migration](schema-evolution-and-migration.md).
The first version should support only hard stage barriers and direct hard resource dependencies.
It should not add numeric priority, declaration-order semantics, arbitrary `before`/`after` hints, or a generic group abstraction.

Promotion requires final stage-identity and namespace rules, barrier semantics for every supported resource action, combined-graph validation, operation-record representation, and tests for:

- stage-only edits that create no resource mutation;
- all predecessor actions succeeding, failing, blocking, or becoming uncertain;
- already-satisfied resources that contribute no executable action;
- combined stage and resource dependency cycles;
- deterministic ordering of independent ready actions;
- path-safety and ownership failures that cannot be bypassed by a barrier; and
- reverse ordering of authorized stale-resource removals;
- topology snapshots after every successful resource commit and partial failure; and
- source-stage rename and stage-only edits without resource migration or mutation.
