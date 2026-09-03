//! Raw user-authored configuration declarations.
//!
//! These DTOs preserve configuration-level strings. Path binding, identifier validation, profile discovery, and filesystem access belong to the resolver and validator boundary.

pub(crate) mod environment_config;
pub(crate) mod profile;
pub(crate) mod runtime_config;

mod schema;
