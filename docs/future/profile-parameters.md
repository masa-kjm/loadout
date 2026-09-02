# Profile Parameters

## Status

This is a non-binding design note.
Profile parameters are not part of v0.2.0 and this document defines no parameter schema or include syntax.

## Purpose

Parameters would make a profile reusable for a constrained set of values without adding a separate template language.
Typical examples are a store-relative source path, a target path, an executable name, or an argument list.
They must not turn a portable desired state into a value assembled implicitly from a machine configuration or process environment.

## Candidate Direction

A future profile could declare typed inputs and an including profile could supply values explicitly.
The current draft direction is an include object with an explicit `with` mapping; an alias or equivalent instance name is required when the same parameterized profile is included more than once with different bindings.

```yaml
includes:
  - id: git-config
    as: personal-git-config
    with:
      source-path: git/personal.gitconfig
```

The exact field names and syntax are intentionally undecided.
The important constraint is that every binding is present in the portable declaration graph and is resolved before the planner receives Resolved Desired.
Configuration files, environment variables, current working directories, and other ambient values must not inject parameter values implicitly.

## Resolution and Identity

Parameters change profile composition: including the same profile twice with different values is not the same as reaching one unparameterized profile twice.
The resolver therefore needs an explicit profile-instance identity in addition to the defining profile ID.

Before this capability is promoted, the final design must define:

- how an instance receives a stable, human-readable identity;
- how fully qualified resource IDs distinguish instances without depending on include traversal position;
- whether changing an alias, binding, source, or target is an identity change or a definition change;
- how include-cycle detection works over parameterized instances;
- whether a profile may include itself with a strictly decreasing or otherwise bounded binding; and
- how diagnostics name both the defining profile and the instantiated profile.

Known state must continue to separate resource identity from its canonical resolved definition hash.
The resolved definition hash must include every value that can affect the resource's desired effect.

## Types and Substitution Safety

The initial type set should be intentionally small and extended only for concrete resource needs.
Candidate types are `store_relative_path`, `target_path`, `string`, and `string_list`.
Substitution is typed resolution, not text templating: a value used as a path must pass the same containment and normalization checks as a literal path.

Parameters must not introduce shell interpolation, arbitrary expressions, conditionals, loops, filesystem reads, or network reads into resolution.
Secrets require a separate secret model and must not be added as ordinary parameter values, because resolved desired state, plans, diagnostics, definition hashes, and state records require redaction and persistence rules.

## Lifecycle Constraints

After resolution, the planner receives the same kind of canonical Resolved Desired set as it does in v0.2.0.
It must not receive parameter syntax or choose bindings.
Changing a binding can cause a normal definition change, relocation, replacement, or conflict only through the resource type's total Desired/Known/Actual transition rules.

Parameter support must not weaken target collision detection.
Two independently instantiated resources that resolve to the same normalized target remain a blocking conflict unless a future resource specification explicitly defines a safe ownership-transfer operation.

## Required Promotion Work

Promotion requires a final schema, canonical instance and resource identity rules, a binding-validation model, definition-hash rules, redaction rules, and tests for repeated inclusion, missing and unknown bindings, type violations, target collisions, instance rename behavior, cycles, and platform-specific path values.
