//! Resolved-path values that cannot represent configuration-level path syntax.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// An absolute, lexically normalized path for the current platform.
///
/// This type establishes only lexical properties.  It does not prove physical containment, entry kind, link safety, or source verification; those facts require filesystem observation at their respective lifecycle boundaries.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ResolvedPath(PathBuf);

impl ResolvedPath {
    /// Validates and lexically normalizes an absolute path on the current platform.
    ///
    /// Home-relative syntax and all relative paths are rejected. A parent component is rejected instead of collapsed, because lexical collapsing cannot establish the physical containment required by file-link safety.
    pub(crate) fn new(path: impl Into<PathBuf>) -> Result<Self, ResolvedPathError> {
        let original = path.into();

        if !original.is_absolute() {
            return Err(ResolvedPathError::NotAbsolute { path: original });
        }

        let mut normalized = PathBuf::new();
        for component in original.components() {
            match component {
                Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
                Component::RootDir => normalized.push(component.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(ResolvedPathError::ContainsParentComponent { path: original });
                }
                Component::Normal(segment) => normalized.push(segment),
            }
        }

        debug_assert!(normalized.is_absolute());
        Ok(Self(normalized))
    }

    /// Returns the normalized path without allowing mutation of the value.
    pub(crate) fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consumes the domain value for a filesystem boundary that needs ownership of the native path buffer.
    pub(crate) fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for ResolvedPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for ResolvedPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

/// The reason a path cannot be represented as a `ResolvedPath`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedPathError {
    NotAbsolute { path: PathBuf },
    ContainsParentComponent { path: PathBuf },
}

impl fmt::Display for ResolvedPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAbsolute { path } => write!(
                formatter,
                "resolved paths must be absolute on the current platform: {}",
                path.display()
            ),
            Self::ContainsParentComponent { path } => write!(
                formatter,
                "resolved paths must not contain a parent ('..') component: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ResolvedPathError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_path_normalizes_current_platform_absolute_paths() {
        let base = std::env::temp_dir().join("loadout-resolved-path-test");
        let path_with_current_directory = base.join(".").join("nested");

        let resolved = ResolvedPath::new(path_with_current_directory).unwrap();

        assert_eq!(resolved.as_path(), base.join("nested"));
    }

    #[test]
    fn resolved_path_rejects_relative_and_home_relative_syntax() {
        assert!(matches!(
            ResolvedPath::new("relative/file"),
            Err(ResolvedPathError::NotAbsolute { .. })
        ));
        assert!(matches!(
            ResolvedPath::new("~/file"),
            Err(ResolvedPathError::NotAbsolute { .. })
        ));
    }

    #[test]
    fn resolved_path_rejects_parent_components_without_collapsing_them() {
        let path_with_parent = std::env::temp_dir()
            .join("loadout-resolved-path-test")
            .join("..")
            .join("outside");

        assert!(matches!(
            ResolvedPath::new(path_with_parent),
            Err(ResolvedPathError::ContainsParentComponent { .. })
        ));
    }

    #[test]
    fn resolved_path_preserves_ownership_of_its_native_buffer_only_at_the_boundary() {
        let path = std::env::temp_dir().join("loadout-resolved-path-test");

        assert_eq!(
            ResolvedPath::new(path.clone()).unwrap().into_path_buf(),
            path
        );
    }
}
