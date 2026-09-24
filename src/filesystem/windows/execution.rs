//! Retained Windows directory context for execution-time path association checks.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, PathBuf};

use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
};

use crate::domain::paths::ResolvedPath;
use crate::domain::{
    actual::{CopyTargetObservation, OtherEntryKind, TargetObservation},
    file_copy::ContentFingerprint,
    file_link::LinkTarget,
};

use super::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, IO_REPARSE_TAG_SYMLINK,
    create_relative_file_no_replace, mark_file_for_deletion_on_close,
    open_relative_file_for_delete, open_relative_no_follow, rename_relative_from_handle,
    require_plain_directory, set_file_symbolic_link_reparse_point,
};

/// A no-follow parent chain whose association with its declared paths can be rechecked.
pub(crate) struct ExecutionTarget {
    root_path: ResolvedPath,
    declared_root: ResolvedPath,
    target_path: ResolvedPath,
    directories: Vec<File>,
    parent_names: Vec<OsString>,
    name: OsString,
}

impl ExecutionTarget {
    /// Opens and retains every directory below `root` that parents `target`.
    pub(crate) fn open(root: &ResolvedPath, target: &ResolvedPath) -> io::Result<Self> {
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
        let root_directory = open_directory(root.as_ref())?;
        require_plain_directory(&root_directory)?;
        let mut directories = vec![root_directory];
        for name in &names {
            let parent = directories.last().expect("root retained");
            directories.push(open_relative_no_follow(parent, name, true)?);
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

    /// Retains both the canonical traversal root and the declared-home spelling.
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

    /// Rewalks declared names without following reparse points and compares directory objects.
    pub(crate) fn check_association(&self) -> io::Result<()> {
        check_root_spelling(&self.root_path)?;
        let declared = ResolvedPath::from_platform_canonicalized(fs::canonicalize(
            self.declared_root.as_ref(),
        )?)
        .map_err(|_| invalid("declared root no longer has a supported canonical spelling"))?;
        if declared != self.root_path {
            return Err(invalid(
                "declared root no longer resolves to its canonical directory",
            ));
        }
        let mut current = open_directory(self.root_path.as_ref())?;
        require_plain_directory(&current)?;
        same_directory(&current, &self.directories[0])?;
        for (name, retained) in self.parent_names.iter().zip(&self.directories[1..]) {
            current = open_relative_no_follow(&current, name, true)?;
            same_directory(&current, retained)?;
        }
        Ok(())
    }

    /// Observes the final entry through the retained parent and brackets it with association checks.
    pub(crate) fn observe(&self, expected: &LinkTarget) -> io::Result<TargetObservation> {
        self.check_association()?;
        let observation = self.observe_name(expected)?;
        self.check_association()?;
        Ok(observation)
    }

    /// Copy publication is fail-closed until Windows-native no-replace and failure-aftermath evidence exists.
    pub(crate) fn copy_source_to_temporary(
        &self,
        _: &ResolvedPath,
        _: &ResolvedPath,
        _: &ContentFingerprint,
    ) -> io::Result<()> {
        Err(copy_unsupported())
    }

    /// Copy publication is fail-closed until Windows-native no-replace and failure-aftermath evidence exists.
    pub(crate) fn publish_copy_no_replace(
        &self,
        _: &ResolvedPath,
        _: &ContentFingerprint,
    ) -> io::Result<()> {
        Err(copy_unsupported())
    }

    /// Copy replacement is fail-closed until Windows-native failure-aftermath evidence exists.
    pub(crate) fn replace_copy_from_temporary(
        &self,
        _: &ResolvedPath,
        _: &ContentFingerprint,
        _: &ContentFingerprint,
    ) -> io::Result<()> {
        Err(copy_unsupported())
    }

    /// Copy removal is fail-closed until Windows-native ownership and aftermath evidence exists.
    pub(crate) fn remove_expected_copy(&self, _: &ContentFingerprint) -> io::Result<()> {
        Err(copy_unsupported())
    }

    /// Copy observation is unavailable until the native no-follow byte-reading boundary is proven.
    pub(crate) fn observe_copy(
        &self,
        _: Option<&ContentFingerprint>,
    ) -> io::Result<CopyTargetObservation> {
        Err(copy_unsupported())
    }

    /// Link-to-copy replacement is fail-closed until Windows-native failure-aftermath evidence exists.
    pub(crate) fn replace_link_with_copy_temporary(
        &self,
        _: &ResolvedPath,
        _: &LinkTarget,
        _: &ContentFingerprint,
    ) -> io::Result<()> {
        Err(copy_unsupported())
    }

    /// Copy-to-link replacement is fail-closed until Windows-native failure-aftermath evidence exists.
    pub(crate) fn create_link_temporary(&self, _: &ResolvedPath, _: &LinkTarget) -> io::Result<()> {
        Err(copy_unsupported())
    }

    /// Copy-to-link replacement is fail-closed until Windows-native failure-aftermath evidence exists.
    pub(crate) fn replace_copy_with_link_temporary(
        &self,
        _: &ResolvedPath,
        _: &ContentFingerprint,
        _: &LinkTarget,
    ) -> io::Result<()> {
        Err(copy_unsupported())
    }

    /// Rechecks the declared-path association and exact file-link value, then retains the checked final entry for deletion.
    /// The returned proof can perform no action other than that deletion.
    #[allow(dead_code)] // S6 connects removal after executor and recovery evidence.
    pub(crate) fn prepare_remove(&self, expected: &LinkTarget) -> io::Result<CheckedRemove<'_>> {
        self.check_association()?;
        let file = open_relative_file_for_delete(self.parent(), &self.name)?;
        require_expected_file_symbolic_link(&file, expected)?;
        Ok(CheckedRemove { target: self, file })
    }

    /// Rechecks association and the final expected link without preparing a mutation.
    #[allow(dead_code)] // S6 recovery cleanup uses this after executor integration.
    pub(crate) fn recheck_expected_link(&self, expected: &LinkTarget) -> io::Result<()> {
        self.check_association()?;
        let file = open_relative_no_follow(self.parent(), &self.name, false)?;
        require_expected_file_symbolic_link(&file, expected)
    }

    /// Rechecks declared-path association and missingness, then retains a proof that can only create the requested link without replacement.
    #[allow(dead_code)] // S6 connects creation after source and executor/recovery evidence.
    pub(crate) fn prepare_create<'a>(
        &'a self,
        source_root: &ResolvedPath,
        link: &'a LinkTarget,
    ) -> io::Result<CheckedCreate<'a>> {
        #[cfg(test)]
        crate::test_support::execution_boundary(
            crate::test_support::ExecutionBoundary::BeforeFinalRecheck,
        )?;
        check_source(source_root, link)?;
        self.check_association()?;
        if self.observe_name(link)? != TargetObservation::Missing {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "create entry is not missing",
            ));
        }
        Ok(CheckedCreate { target: self, link })
    }

    /// Rechecks source, old link and recorded temporary before a same-parent replacement.
    #[allow(dead_code)] // S6 connects replacement after executor and recovery evidence.
    pub(crate) fn prepare_replace<'a>(
        &'a self,
        temporary: &ResolvedPath,
        old: &LinkTarget,
        new: &LinkTarget,
        source_root: &ResolvedPath,
    ) -> io::Result<CheckedReplace<'a>> {
        check_source(source_root, new)?;
        self.check_association()?;
        let old_file = open_relative_no_follow(self.parent(), &self.name, false)?;
        require_expected_file_symbolic_link(&old_file, old)?;
        let temporary_name = self.sibling_name(temporary)?;
        let temporary_file = open_relative_file_for_delete(self.parent(), &temporary_name)?;
        require_expected_file_symbolic_link(&temporary_file, new)?;
        Ok(CheckedReplace {
            target: self,
            temporary_file,
        })
    }

    fn parent(&self) -> &File {
        self.directories.last().expect("root retained")
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

    fn observe_name(&self, expected: &LinkTarget) -> io::Result<TargetObservation> {
        let file = match open_relative_no_follow(self.parent(), &self.name, false) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(TargetObservation::Missing);
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        let attributes = std::os::windows::fs::MetadataExt::file_attributes(&metadata);
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            let tag = reparse_buffer(&file)?;
            if tag[..4] != IO_REPARSE_TAG_SYMLINK.to_le_bytes()
                || attributes & FILE_ATTRIBUTE_DIRECTORY != 0
            {
                return Ok(TargetObservation::OtherEntry {
                    kind: OtherEntryKind::ReparsePoint,
                });
            }
            let link_target = LinkTarget::new(parse_file_symbolic_link_target(&tag)?);
            return Ok(if &link_target == expected {
                TargetObservation::ExpectedLink { link_target }
            } else {
                TargetObservation::OtherLink { link_target }
            });
        }
        if metadata.is_file() {
            Ok(TargetObservation::OtherEntry {
                kind: OtherEntryKind::RegularFile,
            })
        } else if metadata.is_dir() {
            Ok(TargetObservation::OtherEntry {
                kind: OtherEntryKind::Directory,
            })
        } else {
            Ok(TargetObservation::OtherEntry {
                kind: OtherEntryKind::Unsupported,
            })
        }
    }
}

/// A short-lived proof of no-replace creation predicates.
#[must_use]
pub(crate) struct CheckedCreate<'a> {
    target: &'a ExecutionTarget,
    link: &'a LinkTarget,
}

impl CheckedCreate<'_> {
    /// Creates the exact recorded replacement temporary before its separate rename recheck.
    #[allow(dead_code)] // S6 connects replacement after executor and recovery evidence.
    pub(crate) fn attempt_temporary(self) -> io::Result<()> {
        self.attempt()
    }

    /// Sets the requested link value, removing the newly created entry through the same handle if setup fails.
    #[allow(dead_code)] // S6 connects creation after source and executor/recovery evidence.
    pub(crate) fn attempt(self) -> io::Result<()> {
        let Self { target, link } = self;
        #[cfg(test)]
        crate::test_support::execution_boundary(
            crate::test_support::ExecutionBoundary::AfterFinalRecheck,
        )?;
        let file = create_relative_file_no_replace(target.parent(), &target.name)?;
        if let Err(error) = set_file_symbolic_link_reparse_point(&file, link.as_path().as_ref()) {
            let cleanup = mark_file_for_deletion_on_close(&file);
            drop(file);
            return cleanup.and(Err(error));
        }
        drop(file);
        target.check_association()
    }
}

/// A short-lived proof of same-parent replacement predicates.
#[must_use]
pub(crate) struct CheckedReplace<'a> {
    target: &'a ExecutionTarget,
    temporary_file: File,
}

impl CheckedReplace<'_> {
    /// Replaces the final name from the checked temporary without a delete-then-create fallback.
    #[allow(dead_code)] // S6 connects replacement after executor and recovery evidence.
    pub(crate) fn attempt(self) -> io::Result<()> {
        let Self {
            target,
            temporary_file,
        } = self;
        rename_relative_from_handle(&temporary_file, target.parent(), &target.name, true)?;
        drop(temporary_file);
        target.check_association()
    }
}

/// A short-lived proof of the retained-parent removal predicates.
#[must_use]
pub(crate) struct CheckedRemove<'a> {
    target: &'a ExecutionTarget,
    file: File,
}

impl CheckedRemove<'_> {
    /// Deletes only the checked final entry when this owned handle closes.
    #[allow(dead_code)] // S6 connects removal after executor and recovery evidence.
    pub(crate) fn attempt(self) -> io::Result<()> {
        let Self { target, file } = self;
        mark_file_for_deletion_on_close(&file)?;
        drop(file);
        target.check_association()
    }
}

fn reparse_buffer(file: &File) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;

    let mut buffer = vec![0_u8; 16 * 1024];
    let mut returned = 0_u32;
    // SAFETY: the handle belongs to `file` and the output buffer is writable for this synchronous call.
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    if returned < 20 {
        return Err(invalid("reparse data is too short"));
    }
    buffer.truncate(returned as usize);
    Ok(buffer)
}

fn require_expected_file_symbolic_link(file: &File, expected: &LinkTarget) -> io::Result<()> {
    let metadata = file.metadata()?;
    let attributes = std::os::windows::fs::MetadataExt::file_attributes(&metadata);
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 || attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    {
        return Err(invalid("entry is not a file symbolic link"));
    }
    let buffer = reparse_buffer(file)?;
    if buffer[..4] != IO_REPARSE_TAG_SYMLINK.to_le_bytes() {
        return Err(invalid("entry is not a file symbolic link"));
    }
    let observed = LinkTarget::new(parse_file_symbolic_link_target(&buffer)?);
    if &observed != expected {
        return Err(invalid(
            "entry no longer has the expected symbolic-link value",
        ));
    }
    Ok(())
}

fn check_source(root: &ResolvedPath, link: &LinkTarget) -> io::Result<()> {
    let source = ExecutionTarget::open(root, link.as_path())?;
    let file = open_relative_no_follow(source.parent(), &source.name, false)?;
    let metadata = file.metadata()?;
    let attributes = std::os::windows::fs::MetadataExt::file_attributes(&metadata);
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_file() {
        return Err(invalid("source is not a no-follow regular file"));
    }
    source.check_association()
}

fn parse_file_symbolic_link_target(buffer: &[u8]) -> io::Result<ResolvedPath> {
    let data_length = u16::from_le_bytes(buffer[4..6].try_into().unwrap()) as usize;
    if buffer.len() < 8 + data_length {
        return Err(invalid("reparse data length is invalid"));
    }
    let substitute_offset = u16::from_le_bytes(buffer[8..10].try_into().unwrap()) as usize;
    let substitute_length = u16::from_le_bytes(buffer[10..12].try_into().unwrap()) as usize;
    if buffer[16..20] != 0_u32.to_le_bytes() {
        return Err(invalid("relative symbolic links are unsupported"));
    }
    let start = 20 + substitute_offset;
    let end = start
        .checked_add(substitute_length)
        .ok_or_else(|| invalid("reparse substitute target overflows"))?;
    if end > buffer.len() || substitute_length % 2 != 0 {
        return Err(invalid("reparse substitute target is invalid"));
    }
    let units = buffer[start..end]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes(pair.try_into().unwrap()))
        .collect::<Vec<_>>();
    let target = String::from_utf16(&units)
        .map_err(|_| invalid("reparse substitute target is not UTF-16"))?;
    let normal = target
        .strip_prefix(r"\??\UNC\")
        .map(|value| format!(r"\\{value}"))
        .or_else(|| target.strip_prefix(r"\??\").map(ToOwned::to_owned))
        .ok_or_else(|| invalid("reparse substitute target is not an absolute DOS or UNC path"))?;
    ResolvedPath::new(PathBuf::from(normal))
        .map_err(|_| invalid("reparse substitute target is unsupported"))
}

fn open_directory(path: &std::path::Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn check_root_spelling(root: &ResolvedPath) -> io::Result<()> {
    let observed = ResolvedPath::from_platform_canonicalized(fs::canonicalize(root.as_ref())?)
        .map_err(|_| invalid("canonical root no longer has a supported spelling"))?;
    if &observed != root {
        return Err(invalid("canonical root no longer has its recorded path"));
    }
    Ok(())
}

fn same_directory(left: &File, right: &File) -> io::Result<()> {
    let left = directory_identity(left)?;
    let right = directory_identity(right)?;
    if left != right {
        return Err(invalid(
            "retained directory no longer matches its recorded path",
        ));
    }
    Ok(())
}

fn directory_identity(file: &File) -> io::Result<(u32, u32, u32)> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle belongs to `file` and `information` is writable for this synchronous call.
    let result = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut information) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        information.dwVolumeSerialNumber,
        information.nFileIndexHigh,
        information.nFileIndexLow,
    ))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn copy_unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows file-copy execution is unavailable pending native capability evidence",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::os::windows::fs::{symlink_dir, symlink_file};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::ExecutionTarget;
    use crate::domain::actual::TargetObservation;
    use crate::domain::file_link::LinkTarget;
    use crate::domain::paths::ResolvedPath;

    #[test]
    fn association_recheck_rejects_a_replaced_parent_path() {
        let root = fixture_root();
        let parent = root.join("parent");
        let moved = root.join("moved");
        fs::create_dir(&parent).unwrap();
        let root = ResolvedPath::new(root).unwrap();
        let target = ResolvedPath::new(root.as_ref().join("parent/target")).unwrap();
        let context = ExecutionTarget::open(&root, &target).unwrap();
        fs::rename(root.as_ref().join("parent"), &moved).unwrap();
        fs::create_dir(root.as_ref().join("parent")).unwrap();
        assert!(context.check_association().is_err());
        drop(context);
        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    #[test]
    fn target_parent_reparse_point_is_rejected_before_context_creation() {
        let root = fixture_root();
        let outside = root.join("outside");
        let parent = root.join("parent");
        fs::create_dir(&outside).unwrap();
        symlink_dir(&outside, &parent).unwrap();
        let root = ResolvedPath::new(root).unwrap();
        let target = ResolvedPath::new(root.as_ref().join("parent/target")).unwrap();

        assert!(ExecutionTarget::open(&root, &target).is_err());
        assert!(fs::symlink_metadata(outside.join("target")).is_err());

        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    #[test]
    fn source_parent_reparse_point_rejects_create_without_target_mutation() {
        let root = fixture_root();
        let outside = root.join("outside");
        let source_parent = root.join("source-parent");
        let target_path = root.join("target");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("source"), "source").unwrap();
        symlink_dir(&outside, &source_parent).unwrap();
        let root = ResolvedPath::new(root).unwrap();
        let target = ResolvedPath::new(target_path.clone()).unwrap();
        let source =
            LinkTarget::new(ResolvedPath::new(root.as_ref().join("source-parent/source")).unwrap());
        let context = ExecutionTarget::open(&root, &target).unwrap();

        assert!(context.prepare_create(&root, &source).is_err());
        assert!(fs::symlink_metadata(target_path).is_err());

        drop(context);
        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    #[test]
    fn observe_classifies_an_absolute_file_symbolic_link_without_following_it() {
        let root = fixture_root();
        let source = root.join("source");
        let other_source = root.join("other-source");
        let target_path = root.join("target");
        fs::write(&source, "source").unwrap();
        fs::write(&other_source, "other").unwrap();
        if let Err(error) = symlink_file(&source, &target_path) {
            if error.raw_os_error() == Some(1314) {
                fs::remove_dir_all(&root).unwrap();
                return;
            }
            panic!("create fixture file symbolic link: {error}");
        }
        let root = ResolvedPath::new(root).unwrap();
        let target = ResolvedPath::new(target_path).unwrap();
        let source = ResolvedPath::new(source).unwrap();
        let other_source = ResolvedPath::new(other_source).unwrap();
        let context = ExecutionTarget::open(&root, &target).unwrap();

        assert_eq!(
            context.observe(&LinkTarget::new(source.clone())).unwrap(),
            TargetObservation::ExpectedLink {
                link_target: LinkTarget::new(source.clone()),
            }
        );
        assert_eq!(
            context.observe(&LinkTarget::new(other_source)).unwrap(),
            TargetObservation::OtherLink {
                link_target: LinkTarget::new(source),
            }
        );

        drop(context);
        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    #[test]
    fn checked_remove_deletes_only_the_expected_file_symbolic_link() {
        let root = fixture_root();
        let source = root.join("source");
        let target_path = root.join("target");
        fs::write(&source, "source").unwrap();
        if let Err(error) = symlink_file(&source, &target_path) {
            if error.raw_os_error() == Some(1314) {
                fs::remove_dir_all(&root).unwrap();
                return;
            }
            panic!("create fixture file symbolic link: {error}");
        }
        let root = ResolvedPath::new(root).unwrap();
        let target = ResolvedPath::new(target_path).unwrap();
        let source = LinkTarget::new(ResolvedPath::new(source).unwrap());
        let context = ExecutionTarget::open(&root, &target).unwrap();

        context.prepare_remove(&source).unwrap().attempt().unwrap();

        assert_eq!(
            context.observe(&source).unwrap(),
            TargetObservation::Missing
        );
        assert_eq!(
            fs::read_to_string(source.as_path().as_ref()).unwrap(),
            "source"
        );
        drop(context);
        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    #[test]
    fn checked_remove_rejects_a_different_link_without_mutating_it() {
        let root = fixture_root();
        let source = root.join("source");
        let other = root.join("other");
        let target_path = root.join("target");
        fs::write(&source, "source").unwrap();
        fs::write(&other, "other").unwrap();
        if let Err(error) = symlink_file(&other, &target_path) {
            if error.raw_os_error() == Some(1314) {
                fs::remove_dir_all(&root).unwrap();
                return;
            }
            panic!("create fixture file symbolic link: {error}");
        }
        let root = ResolvedPath::new(root).unwrap();
        let target = ResolvedPath::new(target_path.clone()).unwrap();
        let source = LinkTarget::new(ResolvedPath::new(source).unwrap());
        let context = ExecutionTarget::open(&root, &target).unwrap();

        assert!(context.prepare_remove(&source).is_err());

        assert_eq!(fs::read_link(&target_path).unwrap(), other);
        assert_eq!(
            fs::read_to_string(source.as_path().as_ref()).unwrap(),
            "source"
        );
        drop(context);
        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    #[test]
    fn checked_create_never_replaces_and_materializes_the_requested_link() {
        let root = fixture_root();
        let source = root.join("source");
        let target_path = root.join("target");
        fs::write(&source, "source").unwrap();
        let root = ResolvedPath::new(root).unwrap();
        let target = ResolvedPath::new(target_path.clone()).unwrap();
        let source = LinkTarget::new(ResolvedPath::new(source).unwrap());
        let context = ExecutionTarget::open(&root, &target).unwrap();

        context
            .prepare_create(&root, &source)
            .unwrap()
            .attempt()
            .unwrap();

        assert_eq!(
            context.observe(&source).unwrap(),
            TargetObservation::ExpectedLink {
                link_target: source.clone(),
            }
        );
        assert_eq!(fs::read_to_string(target_path).unwrap(), "source");
        assert_eq!(
            context.prepare_create(&root, &source).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        drop(context);
        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    #[test]
    fn checked_create_rejects_a_non_regular_source_before_creating_the_target() {
        let root = fixture_root();
        let source = root.join("source");
        let target_path = root.join("target");
        fs::create_dir(&source).unwrap();
        let root = ResolvedPath::new(root).unwrap();
        let target = ResolvedPath::new(target_path.clone()).unwrap();
        let source = LinkTarget::new(ResolvedPath::new(source).unwrap());
        let context = ExecutionTarget::open(&root, &target).unwrap();

        assert!(context.prepare_create(&root, &source).is_err());
        assert!(fs::symlink_metadata(target_path).is_err());
        drop(context);
        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    #[test]
    fn checked_replace_moves_only_the_expected_temporary_to_the_final_name() {
        let root = fixture_root();
        let old_source = root.join("old-source");
        let new_source = root.join("new-source");
        let target_path = root.join("target");
        let temporary_path = root.join("temporary");
        fs::write(&old_source, "old").unwrap();
        fs::write(&new_source, "new").unwrap();
        if let Err(error) = symlink_file(&old_source, &target_path)
            .and_then(|_| symlink_file(&new_source, &temporary_path))
        {
            if error.raw_os_error() == Some(1314) {
                fs::remove_dir_all(&root).unwrap();
                return;
            }
            panic!("create fixture file symbolic links: {error}");
        }
        let root = ResolvedPath::new(root).unwrap();
        let target = ResolvedPath::new(target_path).unwrap();
        let temporary = ResolvedPath::new(temporary_path).unwrap();
        let old = LinkTarget::new(ResolvedPath::new(old_source).unwrap());
        let new = LinkTarget::new(ResolvedPath::new(new_source).unwrap());
        let context = ExecutionTarget::open(&root, &target).unwrap();

        context
            .prepare_replace(&temporary, &old, &new, &root)
            .unwrap()
            .attempt()
            .unwrap();

        assert_eq!(
            context.observe(&new).unwrap(),
            TargetObservation::ExpectedLink { link_target: new }
        );
        assert!(fs::symlink_metadata(temporary.as_ref()).is_err());
        drop(context);
        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    #[test]
    fn checked_replace_rejects_an_unexpected_temporary_without_mutating_either_link() {
        let root = fixture_root();
        let old_source = root.join("old-source");
        let new_source = root.join("new-source");
        let other_source = root.join("other-source");
        let target_path = root.join("target");
        let temporary_path = root.join("temporary");
        fs::write(&old_source, "old").unwrap();
        fs::write(&new_source, "new").unwrap();
        fs::write(&other_source, "other").unwrap();
        if let Err(error) = symlink_file(&old_source, &target_path)
            .and_then(|_| symlink_file(&other_source, &temporary_path))
        {
            if error.raw_os_error() == Some(1314) {
                fs::remove_dir_all(&root).unwrap();
                return;
            }
            panic!("create fixture file symbolic links: {error}");
        }
        let root = ResolvedPath::new(root).unwrap();
        let target = ResolvedPath::new(target_path.clone()).unwrap();
        let temporary = ResolvedPath::new(temporary_path.clone()).unwrap();
        let old = LinkTarget::new(ResolvedPath::new(old_source.clone()).unwrap());
        let new = LinkTarget::new(ResolvedPath::new(new_source).unwrap());
        let context = ExecutionTarget::open(&root, &target).unwrap();

        assert!(
            context
                .prepare_replace(&temporary, &old, &new, &root)
                .is_err()
        );

        assert_eq!(fs::read_link(&target_path).unwrap(), old_source);
        assert_eq!(fs::read_link(&temporary_path).unwrap(), other_source);
        drop(context);
        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    fn fixture_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "loadout-s6-execution-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        ResolvedPath::from_platform_canonicalized(fs::canonicalize(path).unwrap())
            .unwrap()
            .into_path_buf()
    }
}
