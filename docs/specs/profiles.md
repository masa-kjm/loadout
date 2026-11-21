# Profile Specification

## Scope

This specification defines current profile files, typed parameters, named-instance composition, resource declarations, orchestration declarations, and static validation.
The `file` resource supports file `link`, `copy`, and directory `link`; see [File Links](file-link.md), [File Copies](file-copy.md), and [Directory Links](directory-link.md) for runtime behavior.

## Identifiers

Profile IDs, instance IDs, parameter names, resource IDs, and store IDs MUST match this grammar:

```text
[a-z][a-z0-9-]{0,62}
```

An identifier is case-sensitive.
A profile ID is unique across all discovered profile files.
A resource ID is unique within its defining profile.
An instance ID is unique in the resolved composition of a selected root.

The fully qualified resource ID is `<instance-id>/<resource-id>`.
It is the stable resource identity used in plans, diagnostics, operation records, and Known state.
It does not include a filename, discovery path, target path, or source path.
The selected root instance is named by its profile ID.

## Profile File

One profile file defines one profile.

```yaml
schema_version: 6
id: workstation
includes:
  - id: base
resources:
  git-config:
    type: file
    properties:
      kind: file
      source:
        store: dotfiles
        path: git/config
      target: ~/.gitconfig
      operation: link
```

`schema_version`, `id`, and `resources` are REQUIRED.
`includes` is optional and defaults to an empty list.
Unknown fields are errors at every object level.

`resources` is an object keyed by resource ID.
The resource value MUST contain `type` and `properties`.
The only current type is `file`.
Its accepted `kind` and `operation` pairs are defined in [Current Profile Schema](#current-profile-schema) and its effect-specific properties are defined by [File Links](file-link.md), [File Copies](file-copy.md), and [Directory Links](directory-link.md).
Every normal profile reader rejects a schema version other than `6` before it resolves a source, observes a target, creates a plan, recovers an operation, or writes state.

## Parameters and References

The current profile schema permits an optional `parameters` mapping from a parameter name to exactly one closed type: `string`, `integer`, `boolean`, or `list`.
Omission means that the profile declares no parameters.
A selected root profile MUST declare no parameters.
A profile that declares parameters is not root-eligible; it is validated as a declaration and may be resolved only through an include with complete typed bindings.

A parameter value is a YAML string, integer, boolean, or a `list` as declared.
`null`, float, tagged value, implicit scalar coercion, and mappings other than an allowed reference object are invalid.
A `list` is a YAML sequence, including an empty sequence, of non-recursive string, integer, or boolean scalars.
Nested sequences and references in a list are invalid.
Its canonical representation preserves element type and sequence order, distinguishes strings from integers and booleans, and is independent of mapping key order.

Only a complete permitted resource-property value or a complete include binding value may be a reference:

```yaml
{ parameter: name }
```

No partial string, additional reference field, nested reference, list-element reference, or other reference location is valid.
The initial property contract permits a reference in `properties.kind`, `properties.operation`, `properties.target`, and `properties.source.path` only where that property's literal contract permits the same whole YAML value.
After substitution, existing resource validation owns property type, closed-enum, path normalization, containment, store, source-kind, and target validation.
A list substituted into a property that does not accept a list is invalid.

## Includes

Each include has `id`, optional `as`, `with`, and `stage` fields:

```yaml
includes:
  - id: base
    as: base-for-work
    with:
      theme: { parameter: theme }
```

`id` names a discovered profile.
`as` is optional and defaults to `id`, including when the referenced profile declares parameters.
If the same resolved instance name is reached with a different definition or canonical typed bindings, resolution rejects the conflict; distinct parameterized instances need distinct names. Reaching that instance with different effective phase membership for any of its resources also rejects resolution, even if definitions and bindings agree.
`with` supplies every declared parameter exactly once, supplies no unknown parameter, and each value exactly matches the declared type.
An include-binding reference resolves only from the including instance's already-bound parameters.
A resource-property reference resolves only from its defining instance's already-bound parameters.
Includes do not accept conditional expressions, filesystem paths, other override directives, ambient binding, interpolation, or shell expansion.

Loadout resolves includes into named instances.
Include cycles are rejected over definition-profile reachability, including self-includes.
Repeated paths to the same `as` are deduplicated only when their definition profile ID and canonical typed bindings are equal.
A different definition or binding for the same `as` is invalid.

Include order does not give one resource permission to override another resource.
It is used only to make profile resolution and diagnostics deterministic; it is not an execution-order control.
Profile composition is a set of fully qualified resources; two different resources targeting the same normalized target are a conflict.

## Resolution and Validation

Resolving a root profile produces one Resolved Desired set.
Before a planner receives it, the resolver MUST:

1. Discover and parse all profile files, including syntax of every ordering declaration even if it will be overridden.
2. Reject duplicate profile IDs and invalid identifiers.
3. Resolve includes and reject cycles or missing include IDs.
4. Substitute every permitted whole-value reference in its defining instance scope.
5. Resolve each store reference and resource-local source path.
6. Normalize every resource to its fully qualified ID and resolved paths.
7. Resolve effective phases and direct dependencies against the selected include graph and reject invalid references, cycles, and inconsistent repeated-instance membership.
8. Validate the resulting resource declaration and reject duplicate targets after platform-aware normalization.
9. Produce deterministic resource ordering by fully qualified resource ID.

The resolver MUST NOT read a target, write a file, acquire the state lock, or update state.

## Static Validation

Profile validation checks declarations and safe configuration resolution without mutating the managed environment.
At minimum, it rejects:

- an unsupported schema version;
- an unknown field or resource type;
- an invalid or duplicate identifier;
- an include cycle, missing included profile, invalid instance name, or conflicting repeated instance;
- a parameterized selected root, invalid parameter declaration/value, invalid reference, or invalid include binding;
- a missing store or invalid store root;
- an invalid resource-local source path;
- an unsupported file-resource property;
- a target collision in the resolved desired set; and
- a default profile that does not exist.

Validation may inspect the existence and entry kind of a local-store source.
It must not inspect, create, replace, remove, or otherwise mutate a resource target.

## Resource Identity and Changes

Changing a binding, source, target, kind, or operation while keeping the same fully qualified resource ID is a definition change.
Changing an instance alias or resource ID creates a distinct resource identity.
The lifecycle specification determines whether that change creates, replaces, relocates, removes, or blocks an action.

## Current Profile Schema

This section defines the profile schema for every normal lifecycle, validation, dry-run, and inspection command that reads a profile.
A current profile has `schema_version: 6`; every earlier version and every unknown future version are invalid.
Loadout rejects that version mismatch before it resolves a store source, normalizes a target, observes a target, creates a plan, recovers an operation, or writes state.

The profile object, include objects, resource envelope, and resource `properties` objects reject unknown fields at every level.
`type` remains exactly `file`.
The accepted `(properties.kind, properties.operation)` pairs are exactly:

| `kind` | `operation` | Resolved effect |
| --- | --- | --- |
| `file` | `link` | `file_link` |
| `file` | `copy` | `file_copy` |
| `directory` | `link` | `directory_link` |

`directory` with `copy`, and every other pair, are invalid.
The `directory_link` declaration and resolution rules are defined by [Directory Links](directory-link.md).

Normal commands do not decode an older profile for conversion or provide an implicit migration path.
The explicit schema-3-to-4-to-5-to-6 authoring operation is defined by [Migration](migration.md).
An invalid version is a validation diagnostic, not an input from which a target or existing effect may be inspected or adopted.

## Schema-6 Orchestration

Only the selected root may declare `stages`. It is a mapping from phase names matching the identifier grammar to closed objects with required `after`, a duplicate-free array of phase names from the same mapping. An empty array declares an explicit initial phase. Self references and cycles are invalid. An included profile containing `stages` is invalid even when an enclosing `includes[].stage` overrides every resource membership. The canonical phase ID is `stage/<selected-root-instance-id>/<phase-name>`; it is independent of child identity, include path, and traversal order. A profile with its own `stages` may be selected as a root if it otherwise satisfies root requirements.

An include may declare `stage: <phase-name>`. A resource may declare `stage: <phase-name>` and `depends_on: [<local-resource-id>, ...]`. These are scalar identifiers without `/`; `depends_on` names only resources in the same resolved instance, and a qualified cross-instance spelling is invalid even when it would resolve. Omitted and empty dependency arrays are equivalent. Every declaration is parsed and syntax-checked, including overridden `stage` values. Only a resource's effective phase is checked for existence in the selected root's `stages` mapping. Its effective phase is the first include `stage` encountered on the path from the selected root to its defining instance, otherwise its own resource `stage`, otherwise none. An outer include assignment therefore applies to the whole descendant subtree and wins over inner assignments and resource declarations. For a deduplicated instance, every path must yield the same effective phase for each resource; equal memberships retain ordinary definition and binding deduplication. A child selected alone must satisfy its own root requirements, including any effective resource phases it declares.

Every profile may also declare `resource_dependencies`, a sequence of closed objects with exactly required string fields `before` and `after`. Each endpoint is a local resource ID or a slash-joined path of one or more include aliases followed by a resource ID. The path is relative to the declaring instance and follows actual include edges; it never starts with the selected root ID and contains neither `.` nor `..`. Every endpoint must resolve uniquely inside that instance's include closure. The two endpoints must be in different resolved instances. Unknown, ambiguous, self, same-instance, and duplicate canonical edges are invalid, including duplicates across parent declarations and local `depends_on`. An endpoint path cannot capture a sibling or ancestor outside the declaring closure.

The resolver normalizes both dependency forms to `(before, after)` pairs of fully qualified resource IDs. The Plan and durable direct edge use `origin: "depends_on"` for either form. Declaration location and reference spelling are diagnostic provenance and never durable identity. Read-only Desired presentation retains each resource's effective phase and source, either its own declaration or the winning include path, and each direct dependency's declaration source. For equal effective memberships reached through multiple include paths, an include assignment takes display precedence over an equal resource declaration; among include assignments, choose the lexicographically smallest slash-joined alias path from the selected root whose first assigned `stage` supplies that membership. Equal candidate paths collapse to one source. If no path assigns a phase, display the defining resource declaration. This rule changes only presentation, never phase identity, deduplication, ordering, or ownership. A source location uses stable profile ID, include-alias path, and resource ID; YAML position and include order are not identities. Phase, dependency, and provenance facts are excluded from definition and Desired ownership hashes.

For example, a selected `workstation` root can declare phases `early` and `late` with `late.after: [early]`, include `base` as `base` with `stage: early` and `app` as `app`, and declare `resource_dependencies: [{before: base/git, after: app/editor}]`. The resource `base/git` is effectively in `stage/workstation/early` even if its own declaration says `stage: late`; that overridden spelling must still be syntactically valid. The parent edge resolves to `base/git → app/editor`. If `base` is selected alone, its `late` declaration requires a local root `stages.late`.

`depends_on` and parent dependencies constrain legal current-Desired successor actions behind their predecessors. A predecessor whose required post-condition already holds satisfies its prerequisite without a synthetic action. Phase relations constrain applicable actions through compact barriers in [Lifecycle](lifecycle.md). Unstaged resources belong to no barrier. Declaration, mapping, include, discovery, and lexical resource-name order are not execution-order controls.
