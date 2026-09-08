//! Core contracts and lifecycle implementation for Loadout v0.2.
//!
//! This library target does not yet provide a supported public Rust API.

#[allow(dead_code)]
mod declaration;

#[allow(dead_code)]
mod application;

mod domain;

#[allow(dead_code)]
mod executor;

mod filesystem;

#[allow(dead_code)]
mod inspection;

#[allow(dead_code)]
mod planner;

#[allow(dead_code)]
mod resolver;

#[allow(dead_code)]
mod state;

#[cfg(test)]
mod test_support;

mod cli;
mod loader;

/// Entry point for the Loadout executable; core domain types remain internal.
/// This is not a supported embeddable Rust API.
#[doc(hidden)]
pub fn run_cli() -> std::process::ExitCode {
    cli::run()
}
