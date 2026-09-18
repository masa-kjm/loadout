//! Safe machine-local runtime configuration selection.

use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{declaration::runtime_config::RuntimeConfig, domain::paths::ResolvedPath};

static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

/// The checked, but not yet published, runtime selection update.
#[derive(Debug)]
pub(crate) struct UsePreparation {
    destination: PathBuf,
    prior: Option<String>,
    prior_document: Option<String>,
    candidate: String,
}

impl UsePreparation {
    pub(crate) fn destination(&self) -> &Path {
        &self.destination
    }

    pub(crate) fn prior(&self) -> Option<&str> {
        self.prior.as_deref()
    }

    pub(crate) fn selected_path(&self) -> &str {
        &self.candidate
    }
}

/// Failure to inspect or publish a runtime selection.
#[derive(Debug)]
pub(crate) enum ConfigUseError {
    Invalid {
        path: PathBuf,
        message: String,
    },
    Collision {
        path: PathBuf,
    },
    #[allow(dead_code)]
    // Constructed only on platforms without a documented publication primitive.
    UnsupportedPlatform,
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    InvalidPublished {
        path: PathBuf,
        message: String,
    },
}

impl ConfigUseError {
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Self::Invalid { .. } | Self::Collision { .. } | Self::UnsupportedPlatform => 2,
            Self::Io { .. } | Self::InvalidPublished { .. } => 1,
        }
    }
}

impl fmt::Display for ConfigUseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { path, message } => write!(formatter, "invalid runtime configuration path {}: {message}", path.display()),
            Self::Collision { path } => write!(formatter, "runtime configuration appeared during publication: {}", path.display()),
            Self::UnsupportedPlatform => formatter.write_str("config use requires an atomic runtime-configuration publication primitive on this platform"),
            Self::Io { action, path, source } => write!(formatter, "cannot {action} {}: {source}", path.display()),
            Self::InvalidPublished { path, message } => write!(formatter, "published runtime configuration is invalid at {}: {message}", path.display()),
        }
    }
}

impl std::error::Error for ConfigUseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Validates the selected runtime destination and prepares a complete replacement document.
pub(crate) fn prepare(
    destination: PathBuf,
    selected: &ResolvedPath,
) -> Result<UsePreparation, ConfigUseError> {
    let parent = destination.parent().ok_or_else(|| {
        invalid(
            &destination,
            "runtime configuration has no parent directory",
        )
    })?;
    inspect_existing_parent_prefix(parent)?;
    let (prior, prior_document) = match inspect_destination(&destination)? {
        Some(()) => {
            let document = fs::read_to_string(&destination).map_err(|source| {
                io_error("read existing runtime configuration", &destination, source)
            })?;
            let runtime = RuntimeConfig::parse(&document)
                .map_err(|source| invalid(&destination, source.to_string()))?;
            (runtime.config_path().map(str::to_owned), Some(document))
        }
        None => (None, None),
    };
    let candidate = selected.to_string();
    RuntimeConfig::selected_config(&candidate)
        .map_err(|source| invalid(&destination, source.to_string()))?;
    Ok(UsePreparation {
        destination,
        prior,
        prior_document,
        candidate,
    })
}

/// Publishes a prepared selection after confirmation.
pub(crate) fn publish(preparation: &UsePreparation) -> Result<(), ConfigUseError> {
    publish_with(
        preparation,
        publish_without_replacement,
        publish_replacement,
        || Ok(()),
    )
}

fn publish_with<F, G, H>(
    preparation: &UsePreparation,
    publish_absent: F,
    publish_existing: G,
    after_publication: H,
) -> Result<(), ConfigUseError>
where
    F: FnOnce(&Path, &Path) -> Result<(), ConfigUseError>,
    G: FnOnce(&Path, &Path) -> Result<(), ConfigUseError>,
    H: FnOnce() -> Result<(), ConfigUseError>,
{
    ensure_safe_parent(
        preparation
            .destination
            .parent()
            .expect("prepared destination has a parent"),
    )?;
    let candidate = RuntimeConfig::selected_config(preparation.selected_path())
        .map_err(|source| invalid(preparation.destination(), source.to_string()))?;
    recheck_destination(preparation)?;
    let temporary = create_temporary(
        preparation
            .destination
            .parent()
            .expect("prepared destination has a parent"),
        &candidate,
        preparation.selected_path(),
    )?;
    ensure_safe_parent(
        preparation
            .destination
            .parent()
            .expect("prepared destination has a parent"),
    )?;
    verify_temporary(&temporary, preparation.selected_path())?;
    recheck_destination(preparation)?;
    if preparation.prior_document.is_some() {
        publish_existing(&temporary, preparation.destination())?;
    } else {
        publish_absent(&temporary, preparation.destination())?;
    }
    after_publication()?;
    validate_published(preparation.destination(), preparation.selected_path())?;
    sync_parent(
        preparation
            .destination
            .parent()
            .expect("prepared destination has a parent"),
    )?;
    validate_published(preparation.destination(), preparation.selected_path())?;
    Ok(())
}

fn verify_temporary(path: &Path, selected: &str) -> Result<(), ConfigUseError> {
    validate_published(path, selected).map_err(|error| match error {
        ConfigUseError::InvalidPublished { path, message } => ConfigUseError::Io {
            action: "recheck runtime configuration temporary",
            path,
            source: io::Error::other(message),
        },
        other => other,
    })
}

fn recheck_destination(preparation: &UsePreparation) -> Result<(), ConfigUseError> {
    match (
        &preparation.prior_document,
        inspect_destination(preparation.destination())?,
    ) {
        (None, None) => Ok(()),
        (Some(expected), Some(())) => {
            let observed = fs::read_to_string(preparation.destination()).map_err(|source| {
                io_error(
                    "re-read runtime configuration before publication",
                    preparation.destination(),
                    source,
                )
            })?;
            if &observed == expected {
                Ok(())
            } else {
                Err(invalid(
                    preparation.destination(),
                    "runtime configuration changed before publication",
                ))
            }
        }
        _ => Err(invalid(
            preparation.destination(),
            "runtime configuration changed before publication",
        )),
    }
}

fn inspect_existing_parent_prefix(parent: &Path) -> Result<(), ConfigUseError> {
    for component in absolute_prefixes(parent)? {
        match fs::symlink_metadata(&component) {
            Ok(metadata) => validate_directory(&component, metadata)?,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(io_error(
                    "inspect runtime configuration parent",
                    &component,
                    source,
                ));
            }
        }
    }
    Ok(())
}

fn ensure_safe_parent(parent: &Path) -> Result<(), ConfigUseError> {
    let prefixes = absolute_prefixes(parent)?;
    for component in prefixes {
        match fs::symlink_metadata(&component) {
            Ok(metadata) => validate_directory(&component, metadata)?,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                create_safe_directory(&component)?;
            }
            Err(source) => {
                return Err(io_error(
                    "inspect runtime configuration parent",
                    &component,
                    source,
                ));
            }
        }
    }
    Ok(())
}

fn absolute_prefixes(path: &Path) -> Result<Vec<PathBuf>, ConfigUseError> {
    if !path.is_absolute() {
        return Err(invalid(
            path,
            "runtime configuration parent is not absolute",
        ));
    }
    let mut prefixes = Vec::new();
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if current.is_absolute() {
            prefixes.push(current.clone());
        }
    }
    Ok(prefixes)
}

fn create_safe_directory(path: &Path) -> Result<(), ConfigUseError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(io_error(
                "create runtime configuration parent",
                path,
                source,
            ));
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        io_error(
            "re-inspect created runtime configuration parent",
            path,
            source,
        )
    })?;
    validate_directory(path, metadata)
}

fn validate_directory(path: &Path, metadata: fs::Metadata) -> Result<(), ConfigUseError> {
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid(path, "expected a non-symlink directory"));
    }
    let canonical = fs::canonicalize(path).map_err(|source| {
        io_error(
            "verify runtime configuration parent association",
            path,
            source,
        )
    })?;
    let declared =
        ResolvedPath::new(path.to_owned()).map_err(|source| invalid(path, source.to_string()))?;
    let observed = ResolvedPath::from_platform_canonicalized(canonical)
        .map_err(|source| invalid(path, source.to_string()))?;
    if observed != declared {
        return Err(invalid(
            path,
            "parent is not associated with its declared path",
        ));
    }
    Ok(())
}

fn inspect_destination(path: &Path) -> Result<Option<()>, ConfigUseError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(Some(())),
        Ok(_) => Err(invalid(
            path,
            "expected an absent or regular non-symlink file",
        )),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error("inspect runtime configuration", path, source)),
    }
}

fn create_temporary(
    parent: &Path,
    contents: &str,
    selected: &str,
) -> Result<PathBuf, ConfigUseError> {
    for _ in 0..128 {
        let path = parent.join(format!(
            ".loadout-use-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<(), ConfigUseError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|source| {
                    io_error("create runtime configuration temporary", &path, source)
                })?;
            file.write_all(contents.as_bytes()).map_err(|source| {
                io_error("write runtime configuration temporary", &path, source)
            })?;
            file.sync_all().map_err(|source| {
                io_error("flush runtime configuration temporary", &path, source)
            })?;
            drop(file);
            validate_published(&path, selected)
        })();
        match result {
            Ok(()) => return Ok(path),
            Err(ConfigUseError::Io { source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    Err(ConfigUseError::Io {
        action: "allocate a unique runtime configuration temporary below",
        path: parent.to_owned(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary name collision limit reached",
        ),
    })
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn publish_without_replacement(temporary: &Path, destination: &Path) -> Result<(), ConfigUseError> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    renameat_with(CWD, temporary, CWD, destination, RenameFlags::NOREPLACE)
        .map_err(|source| publication_error(destination, io::Error::from(source)))
}

#[cfg(windows)]
fn publish_without_replacement(temporary: &Path, destination: &Path) -> Result<(), ConfigUseError> {
    move_file(temporary, destination, 0)
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
fn publish_without_replacement(_: &Path, _: &Path) -> Result<(), ConfigUseError> {
    Err(ConfigUseError::UnsupportedPlatform)
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn publish_replacement(temporary: &Path, destination: &Path) -> Result<(), ConfigUseError> {
    fs::rename(temporary, destination)
        .map_err(|source| io_error("replace runtime configuration", destination, source))
}

#[cfg(windows)]
fn publish_replacement(temporary: &Path, destination: &Path) -> Result<(), ConfigUseError> {
    use windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING;
    move_file(temporary, destination, MOVEFILE_REPLACE_EXISTING)
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
fn publish_replacement(_: &Path, _: &Path) -> Result<(), ConfigUseError> {
    Err(ConfigUseError::UnsupportedPlatform)
}

#[cfg(windows)]
fn move_file(temporary: &Path, destination: &Path, flags: u32) -> Result<(), ConfigUseError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: the paths are nul-terminated UTF-16 strings that remain valid for the call.
    if unsafe { MoveFileExW(temporary.as_ptr(), destination_wide.as_ptr(), flags) } == 0 {
        return Err(publication_error(destination, io::Error::last_os_error()));
    }
    Ok(())
}

fn publication_error(path: &Path, source: io::Error) -> ConfigUseError {
    if source.kind() == io::ErrorKind::AlreadyExists {
        ConfigUseError::Collision {
            path: path.to_owned(),
        }
    } else {
        io_error("publish runtime configuration", path, source)
    }
}

fn validate_published(path: &Path, selected: &str) -> Result<(), ConfigUseError> {
    let parent = path
        .parent()
        .ok_or_else(|| ConfigUseError::InvalidPublished {
            path: path.to_owned(),
            message: "runtime configuration has no parent directory".into(),
        })?;
    verify_existing_safe_parent(parent)?;
    inspect_destination(path)?.ok_or_else(|| ConfigUseError::InvalidPublished {
        path: path.to_owned(),
        message: "runtime configuration is absent".into(),
    })?;
    let yaml = fs::read_to_string(path)
        .map_err(|source| io_error("re-open runtime configuration", path, source))?;
    let runtime =
        RuntimeConfig::parse(&yaml).map_err(|source| ConfigUseError::InvalidPublished {
            path: path.to_owned(),
            message: source.to_string(),
        })?;
    if runtime.config_path() != Some(selected) {
        return Err(ConfigUseError::InvalidPublished {
            path: path.to_owned(),
            message: "config_path differs from the selected portable configuration".into(),
        });
    }
    Ok(())
}

/// Rechecks every declared parent without creating a missing component.
fn verify_existing_safe_parent(parent: &Path) -> Result<(), ConfigUseError> {
    for component in absolute_prefixes(parent)? {
        let metadata = match fs::symlink_metadata(&component) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(ConfigUseError::InvalidPublished {
                    path: component,
                    message: "runtime configuration parent disappeared during publication".into(),
                });
            }
            Err(source) => {
                return Err(io_error(
                    "re-inspect runtime configuration parent after publication",
                    &component,
                    source,
                ));
            }
        };
        validate_directory(&component, metadata).map_err(|error| match error {
            ConfigUseError::Invalid { path, message } => {
                ConfigUseError::InvalidPublished { path, message }
            }
            other => other,
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), ConfigUseError> {
    use std::fs::File;
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("flush runtime configuration parent", path, source))
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) -> Result<(), ConfigUseError> {
    Ok(())
}

fn invalid(path: &Path, message: impl Into<String>) -> ConfigUseError {
    ConfigUseError::Invalid {
        path: path.to_owned(),
        message: message.into(),
    }
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> ConfigUseError {
    ConfigUseError::Io {
        action,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        selected: ResolvedPath,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loadout-config-use-test-{}-{}",
                std::process::id(),
                NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let selected = ResolvedPath::new(root.join("portable/config.yaml")).unwrap();
            Self { root, selected }
        }

        fn preparation(&self, destination: &str) -> UsePreparation {
            prepare(self.root.join(destination), &self.selected).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn test_io(path: &Path) -> ConfigUseError {
        io_error(
            "inject publication failure",
            path,
            io::Error::other("injected failure"),
        )
    }

    #[test]
    fn no_replace_collision_preserves_the_external_runtime_file() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation("runtime/loadout.yaml");
        let destination = preparation.destination().to_owned();
        let result = publish_with(
            &preparation,
            |_, path| {
                fs::write(path, "external runtime file").unwrap();
                Err(ConfigUseError::Collision {
                    path: path.to_owned(),
                })
            },
            |_, _| unreachable!("the destination was absent"),
            || Ok(()),
        );

        assert!(matches!(result, Err(ConfigUseError::Collision { .. })));
        assert_eq!(
            fs::read_to_string(destination).unwrap(),
            "external runtime file"
        );
    }

    #[test]
    fn publication_failure_retains_created_parents_and_does_not_create_the_final_file() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation("runtime/nested/loadout.yaml");
        let destination = preparation.destination().to_owned();
        let result = publish_with(
            &preparation,
            |_, path| Err(test_io(path)),
            |_, _| unreachable!("the destination was absent"),
            || Ok(()),
        );

        assert!(matches!(result, Err(ConfigUseError::Io { .. })));
        assert!(fixture.root.join("runtime").is_dir());
        assert!(fixture.root.join("runtime/nested").is_dir());
        assert!(fs::symlink_metadata(destination).is_err());
    }

    #[test]
    fn substituted_temporary_is_rejected_before_publication() {
        let fixture = Fixture::new();
        let parent = fixture.root.join("runtime");
        fs::create_dir(&parent).unwrap();
        let candidate = RuntimeConfig::selected_config(&fixture.selected.to_string()).unwrap();
        let temporary =
            create_temporary(&parent, &candidate, &fixture.selected.to_string()).unwrap();
        fs::write(&temporary, "schema_version: 1\n").unwrap();

        assert!(matches!(
            verify_temporary(&temporary, &fixture.selected.to_string()),
            Err(ConfigUseError::Io { .. })
        ));
        assert_eq!(
            fs::read_to_string(temporary).unwrap(),
            "schema_version: 1\n"
        );
    }

    #[test]
    fn post_publication_substitution_is_reported_and_the_observed_file_is_preserved() {
        let fixture = Fixture::new();
        let preparation = fixture.preparation("runtime/loadout.yaml");
        let destination = preparation.destination().to_owned();
        let result = publish_with(
            &preparation,
            |temporary, path| {
                fs::rename(temporary, path).map_err(|source| io_error("publish", path, source))
            },
            |_, _| unreachable!("the destination was absent"),
            || {
                fs::write(&destination, "schema_version: 1\n").unwrap();
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(ConfigUseError::InvalidPublished { .. })
        ));
        assert_eq!(
            fs::read_to_string(destination).unwrap(),
            "schema_version: 1\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn post_publication_parent_substitution_is_not_reported_as_success() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let preparation = fixture.preparation("runtime/loadout.yaml");
        let destination = preparation.destination().to_owned();
        let parent = destination.parent().unwrap().to_owned();
        let moved_parent = fixture.root.join("moved-runtime");
        let result = publish_with(
            &preparation,
            |temporary, path| {
                fs::rename(temporary, path).map_err(|source| io_error("publish", path, source))
            },
            |_, _| unreachable!("the destination was absent"),
            || {
                fs::rename(&parent, &moved_parent).unwrap();
                symlink(&moved_parent, &parent).unwrap();
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(ConfigUseError::InvalidPublished { .. })
        ));
        assert!(
            fs::symlink_metadata(parent)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(moved_parent.join("loadout.yaml").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn windows_publication_creates_and_replaces_a_runtime_file() {
        let fixture = Fixture::new();
        let first = fixture.preparation("runtime/loadout.yaml");
        publish(&first).unwrap();
        let second = prepare(first.destination().to_owned(), &fixture.selected).unwrap();

        publish(&second).unwrap();
        validate_published(second.destination(), second.selected_path()).unwrap();
    }
}
