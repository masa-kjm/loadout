# Inspection and Authoring Commands

## Status

This is a non-binding design note.
Except for the v0.2.0 `diff` command defined by the [CLI specification](../specs/cli.md), the commands described here are not part of v0.2.0 and this document defines no final command syntax, output format, or exit-status contract.

## Purpose

Loadout will likely need two command families beyond convergence:

- **inspection commands** expose declarations and additional read-only state views; and
- **authoring commands** help create or deliberately edit a Loadout environment.

They must share the canonical resolver, inspector, diagnostics, and safety model with `validate`, `diff`, `plan`, and `apply`.
They must not create a parallel interpretation of profiles, state, or ownership.

## Candidate Inspection Commands

The following commands are likely future capabilities:

- `status` summarizes managed, drifted, pending, conflicting, and uncertain resources;
- `profile list` and `profile show` inspect discovered portable declarations; and
- `resource list` and `resource show` inspect either resolved desired resources for an explicit root profile or Known managed resources, with that scope made unambiguous by the command contract.

`config path`, `config list`, and `config get` are also read-only inspection commands.
They should disclose only configuration information that is safe to print and must redact any future sensitive fields.

The v0.2.0 `diff` command is not a second planner and does not imply a repair action.
A future inspection capability may present Desired-to-Known or Desired-to-Actual views, but each comparison must be labeled precisely and must preserve the no-follow observation and structured diagnostic rules used by planning.

Inspection commands are read-only: they must not acquire the apply lock, write state, reconcile an incomplete operation, create directories, or mutate a target.
If an active operation is uncertain, their output must report that fact rather than hide or repair it.

## Candidate Authoring Commands

`init` may create an initial portable environment layout, and configuration commands such as `config use`, `config reset`, and `config set` may deliberately change machine-local or portable configuration.
They are authoring helpers, not prerequisites for using `--config`, nor part of normal environment convergence.

Schema migration is a separate authoring operation.
It must not be hidden inside `plan`, `apply`, or any read-only inspection command; its safety and durability contract is described in [Schema Evolution and Migration](schema-evolution-and-migration.md).

An authoring command must name the files it will create or edit, refuse implicit overwrite, validate the resulting documents before committing them, and write each file with a documented durable replacement procedure.
It must preserve or explicitly reject comments, formatting, unknown syntax, and concurrent edits; it must never silently discard them.

`config set` needs a typed path-selection and value model rather than unrestricted YAML path mutation.
It must define whether it edits runtime configuration, portable environment configuration, or both, and must reject an ambiguous destination.
`config reset` must not be a shorthand for deleting user-authored portable configuration.

`init` must separate Loadout control metadata from native source assets so that the assets remain useful without Loadout.
It must not initialize a version-control repository, choose a remote, or modify an existing source tree unless a separately confirmed option specifies that effect.

## CLI and Output Constraints

Future command design should keep root-profile selection explicit and avoid ambiguities such as whether a resource name identifies a declaration, an instantiated resource, or a Known resource.
Machine-readable output, pagination, stable human-facing labels, and exit-status classes need their own contract before scripts depend on them.

No authoring or inspection command may bypass planner ownership decisions, amend Known state to hide drift, or convert an unmanaged target into a managed object.
Resource import is a stronger authoring operation and is considered separately in [Resource Import](resource-import.md).

## Required Promotion Work

Promotion requires final command grammars, input and output contracts, scope definitions, redaction rules, concurrent-edit behavior, failure-after-effects behavior, and acceptance tests for no-mutation inspection, invalid configurations, active-operation reporting, safe initialization, and configuration-write recovery.
