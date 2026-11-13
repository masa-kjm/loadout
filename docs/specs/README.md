# Specifications

These documents own the current v0.8.0 observable contracts. [Architecture](../architecture/README.md) owns responsibility boundaries; [Testing Strategy](../development/testing.md) owns verification evidence.

## Read by concern

| Concern | Owning specification |
| --- | --- |
| Runtime and portable configuration, stores, paths | [Configuration](configuration.md) |
| Profile schema, composition, parameters, resolution | [Profiles](profiles.md) |
| File symbolic links and shared target-path safety | [File Links](file-link.md) |
| Regular-file copies and content ownership | [File Copies](file-copy.md) |
| Directory symbolic links | [Directory Links](directory-link.md) |
| Desired/Known/Actual planning, preflight, apply, ordering | [Lifecycle](lifecycle.md) |
| Durable state, operation records, locking, recovery | [State and Recovery: read by task](state-and-recovery.md#read-by-task) |
| Command syntax, confirmation, exit statuses | [CLI](cli.md) |
| Explicit schema-4-to-5 conversion | [Migration](migration.md) |
| Initial portable bundle creation | [Initialization](init.md) |
| Configuration inspection and editing | [Configuration Authoring](config-authoring.md) |
| Read-only status and resource/profile queries | [Inspection](inspection.md) |

For schema boundaries, read [Profiles](profiles.md#current-profile-schema), [State and Recovery](state-and-recovery.md#current-state-schema), and [Migration](migration.md). Runtime and portable configuration versions are owned by [Configuration](configuration.md).
