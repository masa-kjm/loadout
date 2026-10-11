//! Execution of already-planned resource actions.

#[cfg(unix)]
pub(crate) mod directory_link;
#[cfg(windows)]
#[path = "directory_link_windows.rs"]
pub(crate) mod directory_link;
pub(crate) mod file_copy;
pub(crate) mod file_link;
