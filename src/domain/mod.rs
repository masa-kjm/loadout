//! Domain vocabulary shared by the resolver, inspector, planner, and executor.
//!
//! Values in this module are deliberately independent of configuration parsing, filesystem observation, and state persistence.

// The resolver, planner, and persistence boundaries that consume this vocabulary arrive in later slices. Keep it crate-private in the meantime.
#[allow(dead_code)]
pub(crate) mod actual;

#[allow(dead_code)]
pub(crate) mod desired;

#[allow(dead_code)]
pub(crate) mod diagnostic;

#[allow(dead_code)]
pub(crate) mod file_link;

#[allow(dead_code)]
pub(crate) mod hashes;

#[allow(dead_code)]
pub(crate) mod ids;

#[allow(dead_code)]
pub(crate) mod known;

#[allow(dead_code)]
pub(crate) mod paths;

#[allow(dead_code)]
pub(crate) mod plan;
