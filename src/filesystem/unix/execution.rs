//! Retained directory context for file-link checks, attempts and observations.
//!
//! Expected links are physical predicates supplied by the executor, not ownership decisions. No state or resource identity enters this boundary. A checked operation consumes its proof at the syscall; it never chooses another action.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, Stat, fstat, open, openat, readlinkat, renameat, statat,
    symlinkat, unlinkat,
};

use crate::domain::actual::{OtherEntryKind, TargetObservation};
use crate::domain::file_link::LinkTarget;
use crate::domain::paths::ResolvedPath;
use crate::filesystem::normalize_observed_absolute_path;

/// An execution-only parent chain and final name below a canonical root.
/// Handles are retained for every ancestor so an observed parent replacement cannot be hidden by moving the original final parent back beneath it.
pub(crate) struct ExecutionTarget {
    root_path: ResolvedPath,
    declared_root: ResolvedPath,
    target_path: ResolvedPath,
    directories: Vec<OwnedFd>,
    parent_names: Vec<OsString>,
    name: OsString,
}

impl ExecutionTarget {
    pub(crate) fn open(root: &ResolvedPath, target: &ResolvedPath) -> io::Result<Self> {
        #[cfg(test)]
        crate::test_support::assert_target_inspection_allowed();
        let relative = target
            .as_ref()
            .strip_prefix(root.as_ref())
            .map_err(|_| invalid("entry is not below its canonical root"))?;
        let mut names = relative
            .components()
            .map(|component| match component {
                Component::Normal(name) => Ok(name.to_owned()),
                _ => Err(invalid("entry has an invalid root-relative component")),
            })
            .collect::<io::Result<Vec<_>>>()?;
        let name = names
            .pop()
            .ok_or_else(|| invalid("entry must not equal its root"))?;
        // Canonical spelling is an observation, not a promise of continuous association. In particular macOS callers pass /private/... fixtures.
        check_root_spelling(root)?;
        let mut directories = vec![open_directory(root.as_ref())?];
        for name in &names {
            let parent = directories.last().expect("root retained");
            directories.push(open_directory_at(parent, name)?);
        }
        let context = Self {
            root_path: root.clone(),
            declared_root: root.clone(),
            target_path: target.clone(),
            directories,
            parent_names: names,
            name,
        };
        context.check_association()?;
        Ok(context)
    }

    /// Retains the declared-home alias as well as its canonical traversal root.
    /// A changed alias must reject even when the physical directory is intact.
    pub(crate) fn open_with_declared_root(
        root: &ResolvedPath,
        declared_root: &ResolvedPath,
        target: &ResolvedPath,
    ) -> io::Result<Self> {
        let mut context = Self::open(root, target)?;
        context.declared_root = declared_root.clone();
        context.check_association()?;
        Ok(context)
    }

    fn parent(&self) -> &OwnedFd {
        self.directories.last().expect("root retained")
    }

    /// Re-walk the declared canonical path without following links below root, comparing every retained directory object. This does not lock names.
    pub(crate) fn check_association(&self) -> io::Result<()> {
        check_root_spelling(&self.root_path)?;
        if fs::canonicalize(self.declared_root.as_ref())? != self.root_path.as_ref() {
            return Err(invalid(
                "declared root no longer resolves to its canonical directory",
            ));
        }
        let mut current = open_directory(self.root_path.as_ref())?;
        same_directory(&current, &self.directories[0])?;
        for (name, retained) in self.parent_names.iter().zip(&self.directories[1..]) {
            current = open_directory_at(&current, name)?;
            same_directory(&current, retained)?;
        }
        Ok(())
    }

    fn observe_name(
        &self,
        name: &OsString,
        expected: &LinkTarget,
    ) -> io::Result<TargetObservation> {
        let metadata = match statat(self.parent(), name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(metadata) => metadata,
            Err(rustix::io::Errno::NOENT) => return Ok(TargetObservation::Missing),
            Err(error) => return Err(error.into()),
        };
        Ok(match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Symlink => {
                let bytes = readlinkat(self.parent(), name, Vec::new())?;
                let observed = Path::new(std::ffi::OsStr::from_bytes(bytes.as_bytes()));
                let absolute = if observed.is_absolute() {
                    observed.to_path_buf()
                } else {
                    self.target_path
                        .as_ref()
                        .parent()
                        .expect("entry has parent")
                        .join(observed)
                };
                let link_target = LinkTarget::new(
                    normalize_observed_absolute_path(&absolute)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                );
                if &link_target == expected {
                    TargetObservation::ExpectedLink { link_target }
                } else {
                    TargetObservation::OtherLink { link_target }
                }
            }
            FileType::RegularFile => TargetObservation::OtherEntry {
                kind: OtherEntryKind::RegularFile,
            },
            FileType::Directory => TargetObservation::OtherEntry {
                kind: OtherEntryKind::Directory,
            },
            _ => TargetObservation::OtherEntry {
                kind: OtherEntryKind::Unsupported,
            },
        })
    }

    /// A handle-relative observation bracketed by declared-path association checks. Failure is unavailable evidence, never handle-local success.
    pub(crate) fn observe(&self, expected: &LinkTarget) -> io::Result<TargetObservation> {
        #[cfg(test)]
        crate::test_support::execution_boundary(
            crate::test_support::ExecutionBoundary::BeforePostObservation,
        )?;
        self.check_association()?;
        let observation = self.observe_name(&self.name, expected)?;
        self.check_association()?;
        Ok(observation)
    }

    /// Rechecks the source's regular-file and no-follow root-relative predicates in addition to target missingness. The token must be consumed immediately.
    pub(crate) fn prepare_create<'a>(
        &'a self,
        source_root: &ResolvedPath,
        link: &'a LinkTarget,
    ) -> io::Result<CheckedCreate<'a>> {
        before_recheck()?;
        check_source(source_root, link)?;
        self.check_association()?;
        if self.observe_name(&self.name, link)? != TargetObservation::Missing {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "create entry is not missing",
            ));
        }
        Ok(CheckedCreate { target: self, link })
    }

    /// Stale links and recorded temporaries may be dangling: removal needs the expected link value, not an existing source referent.
    #[allow(dead_code)] // S3/S4 connect these primitives after native action evidence.
    pub(crate) fn prepare_remove(&self, expected: &LinkTarget) -> io::Result<CheckedRemove<'_>> {
        before_recheck()?;
        self.check_association()?;
        self.require_link(&self.name, expected)?;
        Ok(CheckedRemove { target: self })
    }

    /// Rechecks the declared-path association and expected final link without preparing a mutation.
    pub(crate) fn recheck_expected_link(&self, expected: &LinkTarget) -> io::Result<()> {
        self.check_association()?;
        self.require_link(&self.name, expected)
    }

    /// The repository supplies the exact recorded sibling; this boundary never generates a temporary name. Both names use one retained parent handle.
    #[allow(dead_code)] // S4 connects replacement after action integration evidence.
    pub(crate) fn prepare_replace<'a>(
        &'a self,
        temporary: &ResolvedPath,
        old: &LinkTarget,
        new: &LinkTarget,
        source_root: &ResolvedPath,
    ) -> io::Result<CheckedReplace<'a>> {
        before_recheck()?;
        let temporary_name = self.sibling_name(temporary)?;
        check_source(source_root, new)?;
        self.check_association()?;
        self.require_link(&self.name, old)?;
        self.require_link(&temporary_name, new)?;
        Ok(CheckedReplace {
            target: self,
            temporary_name,
        })
    }

    fn sibling_name(&self, path: &ResolvedPath) -> io::Result<OsString> {
        if path == &self.target_path || path.as_ref().parent() != self.target_path.as_ref().parent()
        {
            return Err(invalid(
                "replacement temporary must be a distinct same-parent sibling",
            ));
        }
        path.as_ref()
            .file_name()
            .map(ToOwned::to_owned)
            .ok_or_else(|| invalid("temporary has no final component"))
    }

    fn require_link(&self, name: &OsString, expected: &LinkTarget) -> io::Result<()> {
        if !matches!(
            self.observe_name(name, expected)?,
            TargetObservation::ExpectedLink { .. }
        ) {
            return Err(invalid(
                "entry no longer has the expected symbolic-link value",
            ));
        }
        Ok(())
    }
}

/// A short-lived proof of the create predicates; fields and syscall are private.
#[must_use]
pub(crate) struct CheckedCreate<'a> {
    target: &'a ExecutionTarget,
    link: &'a LinkTarget,
}
impl CheckedCreate<'_> {
    /// Marks the semantic boundary after creation of an exact recorded temporary. Naming and allocation remain the repository's responsibility.
    #[allow(dead_code)] // S4 connects this after native replacement integration.
    pub(crate) fn attempt_temporary(self) -> io::Result<()> {
        self.attempt()?;
        #[cfg(test)]
        crate::test_support::execution_boundary(
            crate::test_support::ExecutionBoundary::AfterTemporaryCreation,
        )?;
        Ok(())
    }

    pub(crate) fn attempt(self) -> io::Result<()> {
        before_attempt()?;
        let result = symlinkat(
            self.link.as_path().as_ref(),
            self.target.parent(),
            &self.target.name,
        )
        .map_err(io::Error::from);
        after_attempt(result)
    }
}

#[must_use]
pub(crate) struct CheckedRemove<'a> {
    target: &'a ExecutionTarget,
}
impl CheckedRemove<'_> {
    #[allow(dead_code)] // Consumed by S3/S4; kept executable in primitive tests.
    pub(crate) fn attempt(self) -> io::Result<()> {
        before_attempt()?;
        let result = unlinkat(self.target.parent(), &self.target.name, AtFlags::empty())
            .map_err(io::Error::from);
        after_attempt(result)
    }
}

#[must_use]
pub(crate) struct CheckedReplace<'a> {
    target: &'a ExecutionTarget,
    temporary_name: OsString,
}
impl CheckedReplace<'_> {
    #[allow(dead_code)] // Consumed by S4; no lifecycle capability is enabled here.
    pub(crate) fn attempt(self) -> io::Result<()> {
        before_attempt()?;
        let result = renameat(
            self.target.parent(),
            &self.temporary_name,
            self.target.parent(),
            &self.target.name,
        )
        .map_err(io::Error::from);
        after_attempt(result)
    }
}

fn check_source(root: &ResolvedPath, link: &LinkTarget) -> io::Result<()> {
    let source = ExecutionTarget::open(root, link.as_path())?;
    let metadata = statat(source.parent(), &source.name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return Err(invalid("source is not a no-follow regular file"));
    }
    source.check_association()
}

fn check_root_spelling(root: &ResolvedPath) -> io::Result<()> {
    if fs::canonicalize(root.as_ref())? != root.as_ref() {
        return Err(invalid("canonical root no longer has its recorded path"));
    }
    Ok(())
}
fn open_directory(path: &Path) -> io::Result<OwnedFd> {
    Ok(open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}
fn open_directory_at(parent: &OwnedFd, name: &OsString) -> io::Result<OwnedFd> {
    Ok(openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}
fn same_directory(left: &OwnedFd, right: &OwnedFd) -> io::Result<()> {
    let left: Stat = fstat(left)?;
    let right: Stat = fstat(right)?;
    if left.st_dev != right.st_dev || left.st_ino != right.st_ino {
        return Err(invalid(
            "retained directory no longer matches its recorded path",
        ));
    }
    Ok(())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn before_recheck() -> io::Result<()> {
    #[cfg(test)]
    {
        crate::test_support::assert_mutation_allowed();
        crate::test_support::execution_boundary(
            crate::test_support::ExecutionBoundary::BeforeFinalRecheck,
        )?;
    }
    Ok(())
}
fn before_attempt() -> io::Result<()> {
    #[cfg(test)]
    {
        crate::test_support::assert_mutation_allowed();
        crate::test_support::execution_boundary(
            crate::test_support::ExecutionBoundary::AfterFinalRecheck,
        )?;
    }
    Ok(())
}
fn after_attempt(result: io::Result<()>) -> io::Result<()> {
    #[cfg(test)]
    crate::test_support::execution_boundary(
        crate::test_support::ExecutionBoundary::AfterMutationAttempt,
    )?;
    result
}

#[cfg(test)]
mod tests;
