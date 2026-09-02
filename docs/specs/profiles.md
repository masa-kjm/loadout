# Profile Specification

## Scope

This specification defines profile files, identifiers, include composition, resource declarations, and static validation for v0.2.0.
It defines only the file-link resource declaration; see [File Links](file-link.md) for its runtime behavior.

## Identifiers

Profile IDs, resource IDs, and store IDs MUST match this grammar:

```text
[a-z][a-z0-9-]{0,62}
```

An identifier is case-sensitive.
A profile ID is unique across all discovered profile files.
A resource ID is unique within its defining profile.

The fully qualified resource ID is `<profile-id>/<resource-id>`.
It is the stable resource identity used in plans, diagnostics, operation records, and Known state.
It does not include a filename, discovery path, target path, or source path.

## Profile File

One profile file defines one profile.

```yaml
schema_version: 1
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
The only valid v0.2.0 type is `file`.
The `properties` object for that type is defined in [File Links](file-link.md).

## Includes

Each include is an object containing exactly one field:

```yaml
includes:
  - id: base
```

`id` names a discovered profile.
Includes do not accept parameters, aliases, conditional expressions, filesystem paths, or override directives in v0.2.0.

Loadout resolves includes with a depth-first traversal in the order written.
It adds the including profile after its included profiles.
An include cycle is an error.
When the same profile is reached through multiple paths, it is expanded once.

Include order does not give one resource permission to override another resource.
It is used only to make profile resolution and diagnostics deterministic; it is not an execution-order control.
Profile composition is a set of fully qualified resources; two different resources targeting the same normalized target are a conflict.

## Resolution and Validation

Resolving a root profile produces one Resolved Desired set.
Before a planner receives it, the resolver MUST:

1. Discover and parse all profile files.
2. Reject duplicate profile IDs and invalid identifiers.
3. Resolve includes and reject cycles or missing include IDs.
4. Resolve each store reference and resource-local source path.
5. Normalize every resource to its fully qualified ID and resolved paths.
6. Reject duplicate targets after platform-aware normalization.
7. Produce deterministic resource ordering by fully qualified resource ID.

The resolver MUST NOT read a target, write a file, acquire the state lock, or update state.

## Static Validation

Profile validation checks declarations and safe configuration resolution without mutating the managed environment.
At minimum, it rejects:

- an unsupported schema version;
- an unknown field or resource type;
- an invalid or duplicate identifier;
- an include cycle or missing included profile;
- a missing store or invalid store root;
- an invalid resource-local source path;
- an unsupported file-link property;
- a target collision in the resolved desired set; and
- a default profile that does not exist.

Validation may inspect the existence and entry kind of a local-store source.
It must not inspect, create, replace, remove, or otherwise mutate a resource target.

## Resource Identity and Changes

Changing a resource's source or target while keeping the same fully qualified resource ID is a definition change.
Changing the profile ID or resource ID creates a distinct resource identity.
The lifecycle specification determines whether that change creates, replaces, relocates, removes, or blocks an action.
