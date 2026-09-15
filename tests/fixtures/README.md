# Test Fixtures

This directory contains small, deterministic inputs shared by more than one test layer.

```text
fixtures/
├── config/
│   └── valid/
│       ├── config.yaml
│       ├── profiles/
│       └── stores/
├── file-link/
├── migration/
└── state/
```

`config/valid/` is the canonical minimal v0.2 environment configuration. Its local store is kept inside the fixture tree so tests can copy the complete environment into a disposable directory without using a developer's home directory or repository state.

The `file-link/`, `migration/`, and `state/` directories are reserved for focused fixtures as their corresponding test contracts are added. Migration fixtures must be introduced together with an authoritative migration contract; an empty directory is not itself test evidence.

Fixtures are inputs and remain version-controlled. Generated state, logs, sandbox definitions, and temporary copies belong in ignored test-environment directories instead.