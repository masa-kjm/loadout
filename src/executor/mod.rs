//! Execution of already-planned resource actions.

#[cfg(unix)]
pub(crate) mod directory_link;
#[cfg(windows)]
pub(crate) mod directory_link_windows;
#[cfg(windows)]
pub(crate) use directory_link_windows as directory_link;
pub(crate) mod file_copy;
pub(crate) mod file_link;
