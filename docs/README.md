# Loadout Documentation

The current source-tree contract is v0.8.0. Read the documents that own the area you are changing:

- [Architecture](architecture/README.md) owns the system model, responsibilities, and safety boundaries.
- [Specifications](specs/README.md) own observable behavior, schemas, ownership, lifecycle, state, recovery, and CLI contracts.
- [Testing Strategy](development/testing.md) owns the evidence required to verify those contracts. [Platform Primitive Decisions](development/copy-platform-decision-record.md) records current implementation choices for file-copy publication.

The current resource effects are file links, file copies, and directory links from local stores. For entry points, read [Profiles](specs/profiles.md), [Lifecycle](specs/lifecycle.md), and the applicable [resource specification](specs/README.md). [Migration](specs/migration.md) owns the explicit schema-4-to-5 route; normal readers use only the current schemas.

`future/` contains non-binding work outside v0.8.0. `draft/` contains exploratory material. Neither changes the current contract. Historical behavior belongs to the relevant Git revision or release tag.
