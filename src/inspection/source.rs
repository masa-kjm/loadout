//! Physical containment and regular-file verification for local-store sources.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::domain::paths::{ResolvedPath, ResolvedPathError};

/// A source path that was proven to be a regular file below a physical store root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedSource {
    path: ResolvedPath,
}

impl VerifiedSource {
    /// The verified absolute path used as a file-link source and link target.
    pub(crate) fn path(&self) -> &ResolvedPath {
        &self.path
    }
}

/// Resolves an existing local-store root to its physical directory.
pub(crate) fn resolve_store_root(
    store_root: &Path,
) -> Result<ResolvedPath, SourceVerificationError> {
    let physical_root =
        fs::canonicalize(store_root).map_err(|source| SourceVerificationError::StoreRootIo {
            path: store_root.to_path_buf(),
            source,
        })?;
    let metadata =
        fs::metadata(&physical_root).map_err(|source| SourceVerificationError::StoreRootIo {
            path: physical_root.clone(),
            source,
        })?;
    if !metadata.is_dir() {
        return Err(SourceVerificationError::StoreRootNotDirectory {
            path: physical_root,
        });
    }

    ResolvedPath::new(physical_root).map_err(SourceVerificationError::InvalidResolvedPath)
}

/// Verifies all source components without following links beneath a physical store root.
pub(crate) fn verify_regular_source(
    store_root: &ResolvedPath,
    source_components: &[String],
) -> Result<VerifiedSource, SourceVerificationError> {
    let (final_component, parent_components) = source_components
        .split_last()
        .ok_or(SourceVerificationError::EmptySourcePath)?;
    let mut current = store_root.as_ref().to_path_buf();

    for component in parent_components {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|source| {
            SourceVerificationError::SourceComponentIo {
                path: current.clone(),
                source,
            }
        })?;
        let file_type = metadata.file_type();
        if is_link_or_reparse_point(&metadata) {
            return Err(SourceVerificationError::SourceParentLinkOrReparsePoint { path: current });
        }
        if !file_type.is_dir() {
            return Err(SourceVerificationError::SourceParentNotDirectory { path: current });
        }
    }

    current.push(final_component);
    let metadata = fs::symlink_metadata(&current).map_err(|source| {
        SourceVerificationError::SourceComponentIo {
            path: current.clone(),
            source,
        }
    })?;
    let file_type = metadata.file_type();
    if is_link_or_reparse_point(&metadata) {
        return Err(SourceVerificationError::SourceLinkOrReparsePoint { path: current });
    }
    if !file_type.is_file() {
        return Err(SourceVerificationError::SourceNotRegular { path: current });
    }

    let path = ResolvedPath::new(current).map_err(SourceVerificationError::InvalidResolvedPath)?;
    Ok(VerifiedSource { path })
}

/// Whether metadata identifies an entry that source resolution must not traverse.
pub(crate) fn is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// The reason a local-store source cannot be treated as verified input.
#[derive(Debug)]
pub(crate) enum SourceVerificationError {
    EmptySourcePath,
    StoreRootIo { path: PathBuf, source: io::Error },
    StoreRootNotDirectory { path: PathBuf },
    SourceComponentIo { path: PathBuf, source: io::Error },
    SourceParentLinkOrReparsePoint { path: PathBuf },
    SourceParentNotDirectory { path: PathBuf },
    SourceLinkOrReparsePoint { path: PathBuf },
    SourceNotRegular { path: PathBuf },
    InvalidResolvedPath(ResolvedPathError),
}

impl fmt::Display for SourceVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySourcePath => formatter.write_str("source path must not be empty"),
            Self::StoreRootIo { path, source } => {
                write!(
                    formatter,
                    "cannot access local store root {}: {source}",
                    path.display()
                )
            }
            Self::StoreRootNotDirectory { path } => {
                write!(
                    formatter,
                    "local store root is not a directory: {}",
                    path.display()
                )
            }
            Self::SourceComponentIo { path, source } => {
                write!(
                    formatter,
                    "cannot access source component {}: {source}",
                    path.display()
                )
            }
            Self::SourceParentLinkOrReparsePoint { path } => {
                write!(
                    formatter,
                    "source parent must not be a link or reparse point: {}",
                    path.display()
                )
            }
            Self::SourceParentNotDirectory { path } => {
                write!(
                    formatter,
                    "source parent is not a directory: {}",
                    path.display()
                )
            }
            Self::SourceLinkOrReparsePoint { path } => {
                write!(
                    formatter,
                    "source must not be a link or reparse point: {}",
                    path.display()
                )
            }
            Self::SourceNotRegular { path } => {
                write!(
                    formatter,
                    "source is not a regular file: {}",
                    path.display()
                )
            }
            Self::InvalidResolvedPath(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SourceVerificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StoreRootIo { source, .. } | Self::SourceComponentIo { source, .. } => {
                Some(source)
            }
            Self::InvalidResolvedPath(error) => Some(error),
            Self::EmptySourcePath
            | Self::StoreRootNotDirectory { .. }
            | Self::SourceParentLinkOrReparsePoint { .. }
            | Self::SourceParentNotDirectory { .. }
            | Self::SourceLinkOrReparsePoint { .. }
            | Self::SourceNotRegular { .. } => None,
        }
    }
}
