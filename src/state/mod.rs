//! Durable v0.2 state and operation-progress ownership.
//!
//! The state repository is the only layer that serializes Known facts, owns the exclusive lock, and advances operation status.  It does not inspect resource targets or select lifecycle actions.

mod codec;
pub(crate) mod operation;
pub(crate) mod repository;
