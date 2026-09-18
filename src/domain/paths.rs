//! Validated path values used at resolver and filesystem boundaries.

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
        #[cfg(windows)]
        let original = normalize_windows_path(original)?;

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

    /// Converts an existing path returned by the host canonicalization API into the supported resolved representation.
    ///
    /// On Windows this accepts only the verbatim DOS and UNC spelling emitted by `std::fs::canonicalize`; user declarations and persisted paths must use `new` and cannot use that spelling.
    pub(crate) fn from_platform_canonicalized(
        path: impl Into<PathBuf>,
    ) -> Result<Self, ResolvedPathError> {
        let path = path.into();
        #[cfg(windows)]
        let path = normal_windows_path_from_canonicalize(path)?;
        Self::new(path)
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

#[cfg(windows)]
use std::ffi::OsString;
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::GetLongPathNameW;

#[cfg(windows)]
fn normalize_windows_path(original: PathBuf) -> Result<PathBuf, ResolvedPathError> {
    let raw = original
        .to_str()
        .ok_or_else(|| ResolvedPathError::UnsupportedWindowsPath {
            path: original.clone(),
        })?;
    let normalized_separators = raw.replace('/', "\\");
    if normalized_separators.starts_with(r"\\?\")
        || normalized_separators.starts_with(r"\\.\")
        || normalized_separators.starts_with(r"\\??\")
        || normalized_separators.starts_with(r"\??\")
    {
        return Err(ResolvedPathError::UnsupportedWindowsPath { path: original });
    }

    let path = PathBuf::from(&normalized_separators);
    if !path.is_absolute() {
        return Ok(path);
    }
    let is_normal_prefix = matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::UNC(_, _))
    );
    if !is_normal_prefix || !windows_components_are_supported(&normalized_separators) {
        return Err(ResolvedPathError::UnsupportedWindowsPath { path: original });
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir | Component::ParentDir => {
                return Err(ResolvedPathError::UnsupportedWindowsPath { path: original });
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    Ok(normalized)
}

#[cfg(windows)]
fn normal_windows_path_from_canonicalize(
    canonicalized: PathBuf,
) -> Result<PathBuf, ResolvedPathError> {
    let raw = canonicalized
        .to_str()
        .ok_or_else(|| ResolvedPathError::UnsupportedWindowsPath {
            path: canonicalized.clone(),
        })?;
    let normal = if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(dos) = raw.strip_prefix(r"\\?\") {
        PathBuf::from(dos)
    } else {
        canonicalized
    };
    Ok(long_windows_path(&normal).unwrap_or(normal))
}

#[cfg(windows)]
fn long_windows_path(path: &Path) -> Option<PathBuf> {
    let input = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let required = unsafe { GetLongPathNameW(input.as_ptr(), std::ptr::null_mut(), 0) };
    if required == 0 {
        return None;
    }
    let mut output = vec![0; required as usize];
    let written = unsafe {
        GetLongPathNameW(
            input.as_ptr(),
            output.as_mut_ptr(),
            output.len().try_into().ok()?,
        )
    };
    if written == 0 || written as usize >= output.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(
        &output[..written as usize],
    )))
}

#[cfg(windows)]
fn windows_components_are_supported(path: &str) -> bool {
    let Some(remainder) = path.strip_prefix(r"\\").or_else(|| {
        path.get(3..).filter(|_| {
            path.as_bytes().get(1) == Some(&b':') && path.as_bytes().get(2) == Some(&b'\\')
        })
    }) else {
        return false;
    };
    let mut components = remainder.split('\\').peekable();
    let mut count = 0;
    while let Some(component) = components.next() {
        if component.is_empty() {
            if components.peek().is_none() {
                continue;
            }
            return false;
        }
        count += 1;
        if component == "."
            || component == ".."
            || component.ends_with(' ')
            || component.ends_with('.')
            || component.contains(':')
            || component.contains('<')
            || component.contains('>')
            || component.contains('"')
            || component.contains('|')
            || component.contains('?')
            || component.contains('*')
            || component.chars().any(|character| character <= '\u{001F}')
            || is_reserved_windows_name(component)
        {
            return false;
        }
    }
    count >= if path.starts_with(r"\\") { 2 } else { 0 }
}

#[cfg(windows)]
fn is_reserved_windows_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
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

/// A portable relative path that is safe to resolve beneath a verified store root.
///
/// This value is transient resolver input: it is never part of Resolved Desired.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceRelativePath {
    components: Vec<String>,
}

impl SourceRelativePath {
    /// Validates file-link source declaration syntax without inspecting the filesystem.
    pub(crate) fn parse(raw_path: &str) -> Result<Self, SourceRelativePathError> {
        if raw_path.is_empty()
            || raw_path.starts_with('/')
            || raw_path.starts_with("~/")
            || raw_path.contains('\\')
            || has_windows_drive_prefix(raw_path)
            || Path::new(raw_path).is_absolute()
        {
            return Err(SourceRelativePathError::InvalidSyntax);
        }

        let components = raw_path
            .split('/')
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if components.iter().any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || has_windows_drive_prefix(component)
        }) {
            return Err(SourceRelativePathError::InvalidSyntax);
        }

        Ok(Self { components })
    }

    /// Returns the validated components for no-follow traversal below a store root.
    pub(crate) fn components(&self) -> &[String] {
        &self.components
    }
}

/// Returns whether a path begins with a Windows drive prefix on every host.
///
/// This is lexical validation rather than a host-platform path operation.
pub(crate) fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// The reason a path cannot be represented as a `ResolvedPath`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedPathError {
    NotAbsolute { path: PathBuf },
    ContainsParentComponent { path: PathBuf },
    UnsupportedWindowsPath { path: PathBuf },
}

/// The reason a source declaration cannot be represented as a safe relative path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceRelativePathError {
    InvalidSyntax,
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
            Self::UnsupportedWindowsPath { path } => write!(
                formatter,
                "resolved path is not a supported normal Windows path: {}",
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

        #[cfg(unix)]
        {
            let resolved = ResolvedPath::new(path_with_current_directory).unwrap();
            assert_eq!(resolved.as_path(), base.join("nested"));
        }
        #[cfg(windows)]
        assert!(matches!(
            ResolvedPath::new(path_with_current_directory),
            Err(ResolvedPathError::UnsupportedWindowsPath { .. })
        ));
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

        #[cfg(unix)]
        assert!(matches!(
            ResolvedPath::new(path_with_parent),
            Err(ResolvedPathError::ContainsParentComponent { .. })
        ));
        #[cfg(windows)]
        assert!(matches!(
            ResolvedPath::new(path_with_parent),
            Err(ResolvedPathError::UnsupportedWindowsPath { .. })
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

    #[test]
    fn source_relative_path_rejects_every_forbidden_declaration_syntax() {
        for raw_path in [
            "",
            "/absolute",
            "~/home-relative",
            "a//b",
            "a/./b",
            "a/../b",
            "a\\b",
            "C:config",
            "C:/absolute-on-windows",
        ] {
            assert_eq!(
                SourceRelativePath::parse(raw_path),
                Err(SourceRelativePathError::InvalidSyntax),
                "{raw_path:?} must not be a valid source path"
            );
        }

        assert_eq!(
            SourceRelativePath::parse("git/config")
                .unwrap()
                .components(),
            ["git", "config"]
        );
    }

    #[test]
    fn windows_drive_prefix_is_detected_lexically_on_every_host() {
        for path in ["C:", "C:config", "z:/absolute-on-windows"] {
            assert!(has_windows_drive_prefix(path), "{path:?} must be rejected");
        }
        for path in ["", ":config", "config:drive", "1:config", "git/config"] {
            assert!(
                !has_windows_drive_prefix(path),
                "{path:?} must not be treated as a drive prefix"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_accept_only_normal_dos_and_unc_and_canonicalize_separators() {
        for (input, expected) in [
            (r"C:\Users\Example\file", r"C:\Users\Example\file"),
            ("C:/Users/Example/file", r"C:\Users\Example\file"),
            (r"\\server\share\file", r"\\server\share\file"),
        ] {
            assert_eq!(
                ResolvedPath::new(input).unwrap().as_path(),
                Path::new(expected),
                "{input:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_reject_unsupported_namespaces_and_components() {
        for path in [
            r"C:relative",
            r"\\?\C:\Users\Example\file",
            r"\\?\UNC\server\share\file",
            r"\\.\C:\Users\Example\file",
            r"\\??\C:\Users\Example\file",
            r"C:\Users\.\file",
            r"C:\Users\..\file",
            r"C:\Users\file. ",
            r"C:\Users\CON.txt",
            r"C:\Users\file:stream",
            r"C:\Users\file?.txt",
            "C:\\Users\\file\u{0001}.txt",
            "C:\\Users\\file\u{001F}.txt",
            r"\\server",
        ] {
            assert!(ResolvedPath::new(path).is_err(), "{path:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_equality_remains_case_exact() {
        assert_ne!(
            ResolvedPath::new(r"C:\Users\Example\file").unwrap(),
            ResolvedPath::new(r"C:\Users\example\file").unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    fn canonicalize_boundary_converts_only_os_verbatim_dos_and_unc_output() {
        assert_eq!(
            ResolvedPath::from_platform_canonicalized(r"\\?\C:\Users\Example\file")
                .unwrap()
                .as_path(),
            Path::new(r"C:\Users\Example\file")
        );
        assert_eq!(
            ResolvedPath::from_platform_canonicalized(r"\\?\UNC\server\share\file")
                .unwrap()
                .as_path(),
            Path::new(r"\\server\share\file")
        );
        assert!(ResolvedPath::from_platform_canonicalized(r"\\?\Volume{1234}\file").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn canonicalize_boundary_restores_an_existing_path_to_its_long_normal_spelling() {
        let temporary = std::env::temp_dir();
        let canonicalized = std::fs::canonicalize(&temporary).unwrap();

        assert_eq!(
            ResolvedPath::from_platform_canonicalized(canonicalized).unwrap(),
            ResolvedPath::new(temporary).unwrap()
        );
    }
}
