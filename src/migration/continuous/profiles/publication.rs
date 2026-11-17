//! Safe publication of one fully validated profile migration candidate.

use super::{PlanError, PlannedDocument, profile_facts};
use crate::domain::paths::ResolvedPath;
use crate::filesystem::is_link_or_reparse_point;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::migration::continuous) enum PublicationFault {
    Replace,
    AfterReplace,
}

#[cfg(test)]
thread_local! {
    static PUBLICATION_FAULT: std::cell::Cell<Option<PublicationFault>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(in crate::migration::continuous) fn fail_once_at(point: PublicationFault) {
    PUBLICATION_FAULT.with(|slot| slot.set(Some(point)));
}

#[cfg(test)]
fn fail_at(point: PublicationFault) -> Result<(), PlanError> {
    PUBLICATION_FAULT.with(|slot| {
        if slot.get() == Some(point) {
            slot.set(None);
            Err(PlanError::Invalid(format!(
                "injected profile publication failure at {point:?}"
            )))
        } else {
            Ok(())
        }
    })
}

pub(in crate::migration::continuous) fn publish(
    document: &PlannedDocument,
) -> Result<(), PlanError> {
    let path = &document.path;
    let parent = path
        .parent()
        .ok_or_else(|| PlanError::UnsafePath(path.clone()))?;
    check_parent(parent)?;
    check_file(path, &document.source)?;
    validate_candidate_bytes(document.target_generation, &document.candidate)?;
    let temporary = allocate_temporary(parent)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    file.write_all(&document.candidate)
        .map_err(|source| io_error(&temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error(&temporary, source))?;
    drop(file);
    check_parent(parent)?;
    check_file(&temporary, &document.candidate)?;
    validate_candidate_bytes(
        document.target_generation,
        &fs::read(&temporary).map_err(|source| io_error(&temporary, source))?,
    )?;
    check_parent(parent)?;
    check_file(path, &document.source)?;
    check_file(&temporary, &document.candidate)?;
    #[cfg(test)]
    let replacement = fail_at(PublicationFault::Replace)
        .and_then(|_| replace(&temporary, path).map_err(|source| io_error(path, source)));
    #[cfg(not(test))]
    let replacement = replace(&temporary, path).map_err(|source| io_error(path, source));
    if let Err(error) = replacement {
        let _ = check_parent(parent).and_then(|_| check_file(path, &document.source));
        return Err(error);
    }
    #[cfg(test)]
    fail_at(PublicationFault::AfterReplace)?;
    sync_parent(parent)?;
    check_parent(parent)?;
    check_file(path, &document.candidate)?;
    validate_candidate_bytes(
        document.target_generation,
        &fs::read(path).map_err(|source| io_error(path, source))?,
    )
}

fn validate_candidate_bytes(generation: u32, bytes: &[u8]) -> Result<(), PlanError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| PlanError::Invalid("profile candidate is not UTF-8".to_owned()))?;
    profile_facts(generation, text).map(|_| ())
}

fn check_parent(parent: &Path) -> Result<(), PlanError> {
    if !parent.is_absolute() {
        return Err(PlanError::UnsafePath(parent.to_path_buf()));
    }
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        if !current.is_absolute() {
            continue;
        }
        let metadata =
            fs::symlink_metadata(&current).map_err(|source| io_error(&current, source))?;
        if !metadata.is_dir() || is_link_or_reparse_point(&metadata) {
            return Err(PlanError::UnsafePath(current));
        }
        check_association(&current)?;
    }
    Ok(())
}

fn check_association(path: &Path) -> Result<(), PlanError> {
    let declared = ResolvedPath::from_platform_existing(path.to_path_buf())
        .map_err(|_| PlanError::UnsafePath(path.to_path_buf()))?;
    let canonical = fs::canonicalize(path).map_err(|source| io_error(path, source))?;
    let observed = ResolvedPath::from_platform_canonicalized(canonical)
        .map_err(|_| PlanError::UnsafePath(path.to_path_buf()))?;
    if declared != observed {
        return Err(PlanError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn check_file(path: &Path, expected: &[u8]) -> Result<(), PlanError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.is_file() || is_link_or_reparse_point(&metadata) {
        return Err(PlanError::UnsafePath(path.to_path_buf()));
    }
    check_association(path)?;
    if fs::read(path).map_err(|source| io_error(path, source))? != expected {
        return Err(PlanError::ConfirmedSnapshotChanged);
    }
    Ok(())
}

fn allocate_temporary(parent: &Path) -> Result<PathBuf, PlanError> {
    for _ in 0..128 {
        let nonce = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".loadout-profile-continuous-{}-{nonce}.tmp",
            std::process::id()
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(PlanError::Invalid(
        "migration temporary names exhausted".to_owned(),
    ))
}

#[cfg(unix)]
fn replace(temporary: &Path, final_path: &Path) -> io::Result<()> {
    fs::rename(temporary, final_path)
}

#[cfg(windows)]
fn replace(temporary: &Path, final_path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};
    let source = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = final_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), PlanError> {
    fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error(parent, source))
}

#[cfg(windows)]
fn sync_parent(_parent: &Path) -> Result<(), PlanError> {
    Ok(())
}

fn io_error(path: &Path, source: io::Error) -> PlanError {
    PlanError::Io {
        path: path.to_path_buf(),
        source,
    }
}
