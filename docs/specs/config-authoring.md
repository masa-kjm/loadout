# Configuration Authoring Specification

## Scope

This specification defines the v0.3.0 configuration inspection and authoring commands.
They inspect or edit machine-local runtime configuration and portable environment configuration.
They are not lifecycle commands and do not materialize resources.

## Commands

~~~
loadout config path [--config <path>]
loadout config list [--config <path>]
loadout config get [--config <path>] <field>
loadout config use <path> [--yes]
loadout config set [--config <path>] <field> <value> [--yes]
~~~

config path reports the platform runtime loadout.yaml path and the effective portable configuration path.
With --config, the reported portable path is that explicit path.
It does not require either file to exist.

config list and config get read the effective portable environment configuration, or the --config path when supplied.
They parse and validate the complete document before reporting values.
config get accepts only a field path defined by [Field Paths](#field-paths-and-values).

config use changes only the machine-local runtime configuration.
It validates the selected portable environment configuration first, then writes its resolved path as the runtime config_path.
It never edits the selected portable configuration.
A relative config use path is resolved from the current working directory, exactly as an explicit --config path is.
An absolute path and a leading ~/ path are resolved under the configuration-level path rules in Configuration.

config set changes only one portable environment configuration.
With --config, that option selects its exact target.
Without it, Loadout resolves the effective configuration through the runtime selection rules in [Configuration](configuration.md).
It never edits loadout.yaml.

## Field Paths and Values

config get and config set use schema-aware dot paths.
They do not implement YAML Pointer, arbitrary YAML traversal, array indexing, or edits across documents.

The v0.3.0 allowed scalar paths are:

- default_profile, whose value is a profile ID; and
- stores.<store-id>.properties.path, whose value is a configuration-level path for a local store.

config list reports all supported fields and their typed values.
It does not print fields that are not part of the supported schema.
Collection-valued fields, including profile_discovery.paths, have no config set operation in v0.3.0.

An unknown field, an invalid store ID, a store other than local, a value of the wrong type, or an invalid resolved path is an input error.
config set applies the one typed change to the parsed document and validates the complete resulting document, including profile discovery and stores, before any write.

## Read-only Behavior

config path, config list, and config get do not acquire the state lock, inspect a resource target, create a directory, write state, recover an operation, or mutate any filesystem entry.
They return status 0 on success, 2 for invalid input or an invalid/missing required configuration document, and 1 for an I/O failure.

The output identifies the inspected file.
config get identifies the field and typed resolved value.
The default human-readable output is not a machine-readable API.

## Write Confirmation

Before a normal config use or config set, Loadout displays the exact destination, the prior value or absence, and the resulting value.
When standard input and standard error are terminals, it proceeds only after an affirmative confirmation.
When the session is non-interactive, --yes is required.
--yes bypasses confirmation only; it does not bypass parsing, validation, collision checks, or immediate rechecks.

Declining or omitting required confirmation returns status 2 without a new write.

## Runtime Selection Write

If the runtime loadout.yaml is absent, config use may create its parent directory and a complete version-1 runtime configuration containing only schema_version and config_path.
If it exists, it must be a regular non-symlink file with a safe parent path and a valid version-1 runtime configuration.
config use rejects another entry kind, an unsafe parent, an unsupported schema, or an invalid existing document without mutation.

The selected path is stored in normalized absolute form.
The path is first resolved and the selected portable configuration is parsed and validated.
The runtime file is then published by the [authoring publication protocol](#authoring-publication-protocol).

### Runtime Parent Creation

config use may create only missing directory components needed to contain the platform runtime loadout.yaml.
Before creating the first component, and immediately before creating each later component, it must no-follow inspect every existing component and prove that it is a directory associated with the required declared path.
It creates missing components one at a time from the nearest existing safe ancestor toward the runtime file.
After each creation, it must re-observe the created component without following and prove the required path association before continuing.

An observed non-directory, symlink, junction, reparse point, unavailable path, or unprovable association rejects the command with status 2 before the next creation or runtime-file publication.
An I/O or durability failure returns status 1.
If an external writer creates the component after the last absence recheck, Loadout may continue only after a fresh no-follow observation proves that the resulting component is a safe directory at the required path.
It must otherwise stop without publishing the runtime file.

On any failure after a runtime parent component was created, Loadout retains every created directory.
It must not remove a created directory during failure cleanup because an external writer may have substituted or populated it.
The retained directory is not state, does not imply ownership of its contents, and does not authorize a later deletion.

## Portable Configuration Write

config set requires an existing regular non-symlink portable configuration file with a safe parent path.
It does not create a configuration document, store, profile, or resource declaration.
It rejects an unsafe path, an unsupported schema, an invalid document, or an invalid proposed document without mutation.

The command preserves the document's schema version and the semantic values of every field other than the selected typed field.
It must preserve comments, whitespace, quoting, key order, anchors, aliases, and other YAML presentation outside the edited value.
If it cannot prove that preservation for the input document, it rejects without writing.

## Authoring Publication Protocol

The write commands are outside Desired/Known/Actual and do not acquire the apply lock, write state, modify a store, inspect or modify a resource target, or adopt ownership.

Before publication, Loadout must:

1. no-follow inspect the destination and every required parent;
2. capture and validate the exact current source document or absent-destination precondition;
3. create a unique temporary sibling owned by Loadout;
4. write the complete candidate document, flush it where supported, then reopen and validate it;
5. immediately recheck the final destination and its required path association; and
6. publish with the applicable primitive defined below.

If an immediate recheck fails, the command performs no final replacement or creation.
After a failed publication or post-publication verification, it preserves the observed final entry and may clean up only a temporary entry it can prove remains its own.
It returns status 1 for an I/O, durability, publication, or unprovable post-publication failure.

The protocol does not claim atomic identity comparison with external writers.
An external entry substituted after the last recheck may be replaced where the platform primitive permits replacement.
Loadout verifies the required final document and path association before reporting success, but does not claim uninterrupted containment or detection of every race.

### Absent Destination Publication

When the final destination is recorded as absent, publication uses a platform no-replace primitive from the unique sibling temporary entry.
The primitive must either create the final name from that entry or report that the destination already exists; it must never replace an entry that wins the race.
If the platform does not provide this guarantee, the command returns status 2 before creating the temporary entry or changing the destination.
An existing entry observed at publication is preserved and returns status 2.

### Existing Destination Publication

When the final destination is a validated regular file, publication uses one same-filesystem name-replacement primitive.
The primitive must preserve the prior final file if replacement itself fails.
It must not delete the existing file and then create an unrelated replacement.
If the platform cannot provide this failure guarantee, the command returns status 2 before creating the temporary entry or changing the destination.
After an attempted replacement, Loadout reopens the final path without following and validates the expected complete document.
A missing, unexpected, or unprovable final observation returns status 1 and preserves the observed entry.

## Non-Goals

v0.3.0 provides no config reset, no generic YAML editor, no list or mapping structural edit, no portable profile or resource-declaration editing, no schema migration, and no multi-document transaction.
