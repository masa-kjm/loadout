# Testing Evidence for loadout Reviews

Choose the narrowest test layer that can prove the contract, then require a higher-level test when the user-visible behavior or public API changes. Do not accept a private unit test as the sole evidence for an observable CLI or compatibility contract.

| Test layer | Suitable evidence |
| --- | --- |
| Pure domain tests | Normalization, validation, state classification, planning, and deterministic ordering without I/O. |
| Filesystem contract tests | No-follow observation, containment, link operations, and forbidden-mutation protection in a temporary filesystem. |
| State repository durability tests | Locking, atomic commits, schema validation, operation progress, and recovery with fault injection. |
| Executor integration tests | Resolved inputs through filesystem execution, verification, and Known-state commit. |
| CLI acceptance tests | Arguments, confirmation, dry-run, output categories, and exit statuses in an isolated process. |
| Platform conformance tests | Real Unix or Windows link, reparse-point, locking, and replacement behavior. |

## Review rules

- A changed CLI command needs CLI-level evidence unless the change is provably unreachable to users.
- A changed serialized contract needs a durability or compatibility fixture when it represents a supported contract.
- A private implementation refactor may use focused internal tests when it leaves all observable contracts unchanged.
- Do not broaden the library's public API only to make a test compile.
- Keep fixtures for intentional stable contracts, not incidental formatting or implementation details.

For filesystem mutations, seek the applicable successful-operation, zero-mutation dry-run, protected-existing-target, containment-failure, and failure-aftermath cases. A test that checks only an error code is insufficient when the contract constrains retained filesystem or metadata state.
