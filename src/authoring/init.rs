//! Safe creation of the initial portable environment bundle.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::declaration::environment_config::EnvironmentConfig;
use crate::declaration::profile::ProfileDeclaration;

#[cfg(unix)]
use std::fs::File;

const CONTROL_DIRECTORY: &str = ".loadout";
const CONFIG_YAML: &str = "schema_version: 1\ndefault_profile: base\n\nprofile_discovery:\n  paths:\n    - ./profiles\n\nstores:\n  native:\n    type: local\n    path: ..\n";
const BASE_YAML: &str = "schema_version: 1\nid: base\nresources: {}\n";

static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);

/// The observable result of an `init` invocation.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct InitReport {
    root: PathBuf,
    dry_run: bool,
}

impl InitReport {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn dry_run(&self) -> bool {
        self.dry_run
    }
}

/// Failure to create an initial portable environment bundle.
#[derive(Debug)]
pub(crate) enum InitError {
    AlreadyExists {
        path: PathBuf,
    },
    #[allow(dead_code)]
    // Constructed only on targets without an atomic no-replace directory publish primitive.
    UnsupportedPlatform,
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    InvalidGeneratedBundle {
        path: PathBuf,
        message: String,
    },
}

impl InitError {
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Self::AlreadyExists { .. } | Self::UnsupportedPlatform => 2,
            Self::Io { .. } | Self::InvalidGeneratedBundle { .. } => 1,
        }
    }
}

impl fmt::Display for InitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists { path } => write!(
                formatter,
                "refusing to initialize because control directory already exists: {}",
                path.display()
            ),
            Self::UnsupportedPlatform => formatter.write_str(
                "init requires a platform primitive that publishes a directory without replacing an existing entry",
            ),
            Self::Io {
                action,
                path,
                source,
            } => write!(formatter, "cannot {action} {}: {source}", path.display()),
            Self::InvalidGeneratedBundle { path, message } => {
                write!(formatter, "generated bundle is invalid at {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for InitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Creates `.loadout` below `current_directory`, or reports its intended path in dry-run mode.
pub(crate) fn initialize(current_directory: &Path, dry_run: bool) -> Result<InitReport, InitError> {
    initialize_with(
        current_directory,
        dry_run,
        publish_without_replacement,
        &mut |_| Ok(()),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InitFaultPoint {
    WriteConfig,
    WriteBaseProfile,
    FlushConfig,
    FlushBaseProfile,
    FlushProfilesDirectory,
    FlushStagingDirectory,
    FlushCurrentDirectory,
    ValidateStaging,
}

fn initialize_with<F>(
    current_directory: &Path,
    dry_run: bool,
    publish: F,
    fault: &mut dyn FnMut(InitFaultPoint) -> io::Result<()>,
) -> Result<InitReport, InitError>
where
    F: FnOnce(&Path, &Path) -> Result<(), InitError>,
{
    let root = current_directory.join(CONTROL_DIRECTORY);
    ensure_absent(&root)?;
    if dry_run {
        return Ok(InitReport {
            root,
            dry_run: true,
        });
    }

    let staging = create_staging_directory(current_directory)?;
    build_and_validate(&staging, fault)?;
    publish(&staging, &root)?;
    validate_generated_bundle(&root)?;
    sync_directory(
        current_directory,
        InitFaultPoint::FlushCurrentDirectory,
        fault,
    )?;
    Ok(InitReport {
        root,
        dry_run: false,
    })
}

fn ensure_absent(path: &Path) -> Result<(), InitError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(InitError::AlreadyExists {
            path: path.to_path_buf(),
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect control directory", path, source)),
    }
}

fn create_staging_directory(current_directory: &Path) -> Result<PathBuf, InitError> {
    for _ in 0..128 {
        let staging = current_directory.join(format!(
            ".loadout-init-{}-{}",
            std::process::id(),
            NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&staging) {
            Ok(()) => return Ok(staging),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(io_error("create staging directory", &staging, source)),
        }
    }
    Err(InitError::Io {
        action: "allocate a unique staging directory below",
        path: current_directory.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "staging name collision limit reached",
        ),
    })
}

fn build_and_validate(
    staging: &Path,
    fault: &mut dyn FnMut(InitFaultPoint) -> io::Result<()>,
) -> Result<(), InitError> {
    let profiles = staging.join("profiles");
    fs::create_dir(&profiles)
        .map_err(|source| io_error("create profiles directory", &profiles, source))?;
    write_durable_file(
        &staging.join("config.yaml"),
        CONFIG_YAML,
        InitFaultPoint::WriteBaseProfile,
        InitFaultPoint::FlushConfig,
        fault,
    )?;
    write_durable_file(
        &profiles.join("base.yaml"),
        BASE_YAML,
        InitFaultPoint::WriteConfig,
        InitFaultPoint::FlushBaseProfile,
        fault,
    )?;
    sync_directory(&profiles, InitFaultPoint::FlushProfilesDirectory, fault)?;
    sync_directory(staging, InitFaultPoint::FlushStagingDirectory, fault)?;
    fault(InitFaultPoint::ValidateStaging)
        .map_err(|source| io_error("validate generated bundle", staging, source))?;
    validate_generated_bundle(staging)
}

fn write_durable_file(
    path: &Path,
    contents: &str,
    write_fault: InitFaultPoint,
    flush_fault: InitFaultPoint,
    fault: &mut dyn FnMut(InitFaultPoint) -> io::Result<()>,
) -> Result<(), InitError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error("create generated file", path, source))?;
    fault(write_fault).map_err(|source| io_error("write generated file", path, source))?;
    file.write_all(contents.as_bytes())
        .map_err(|source| io_error("write generated file", path, source))?;
    fault(flush_fault).map_err(|source| io_error("flush generated file", path, source))?;
    file.sync_all()
        .map_err(|source| io_error("flush generated file", path, source))
}

fn validate_generated_bundle(staging: &Path) -> Result<(), InitError> {
    let config_path = staging.join("config.yaml");
    let config = fs::read_to_string(&config_path)
        .map_err(|source| io_error("re-open generated configuration", &config_path, source))?;
    if config != CONFIG_YAML {
        return Err(InitError::InvalidGeneratedBundle {
            path: config_path,
            message: "contents differ from the initial environment configuration".into(),
        });
    }
    EnvironmentConfig::parse(&config).map_err(|source| InitError::InvalidGeneratedBundle {
        path: config_path,
        message: source.to_string(),
    })?;

    let profile_path = staging.join("profiles/base.yaml");
    let profile = fs::read_to_string(&profile_path)
        .map_err(|source| io_error("re-open generated profile", &profile_path, source))?;
    if profile != BASE_YAML {
        return Err(InitError::InvalidGeneratedBundle {
            path: profile_path,
            message: "contents differ from the initial base profile".into(),
        });
    }
    ProfileDeclaration::parse(&profile).map_err(|source| InitError::InvalidGeneratedBundle {
        path: profile_path,
        message: source.to_string(),
    })?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn publish_without_replacement(staging: &Path, root: &Path) -> Result<(), InitError> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    ensure_absent(root)?;
    renameat_with(CWD, staging, CWD, root, RenameFlags::NOREPLACE)
        .map_err(|source| publication_error(root, io::Error::from(source)))
}

#[cfg(windows)]
fn publish_without_replacement(staging: &Path, root: &Path) -> Result<(), InitError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    ensure_absent(root)?;
    let staging = staging
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let root_wide = root
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: both paths are nul-terminated UTF-16 strings that remain valid for the call.
    if unsafe { MoveFileExW(staging.as_ptr(), root_wide.as_ptr(), 0) } == 0 {
        return Err(publication_error(root, io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
fn publish_without_replacement(_: &Path, _: &Path) -> Result<(), InitError> {
    Err(InitError::UnsupportedPlatform)
}

#[cfg(unix)]
fn sync_directory(
    path: &Path,
    fault_point: InitFaultPoint,
    fault: &mut dyn FnMut(InitFaultPoint) -> io::Result<()>,
) -> Result<(), InitError> {
    fault(fault_point).map_err(|source| io_error("flush directory", path, source))?;
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("flush directory", path, source))
}

#[cfg(not(unix))]
fn sync_directory(
    _: &Path,
    fault_point: InitFaultPoint,
    fault: &mut dyn FnMut(InitFaultPoint) -> io::Result<()>,
) -> Result<(), InitError> {
    fault(fault_point)
        .map_err(|source| io_error("flush directory", Path::new(".loadout"), source))?;
    Ok(())
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> InitError {
    InitError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

fn publication_error(path: &Path, source: io::Error) -> InitError {
    if source.kind() == io::ErrorKind::AlreadyExists {
        InitError::AlreadyExists {
            path: path.to_path_buf(),
        }
    } else {
        io_error("publish control directory", path, source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loadout-init-test-{}-{}",
                std::process::id(),
                NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn creates_a_valid_complete_bundle() {
        let fixture = Fixture::new();
        let report = initialize(&fixture.root, false).unwrap();

        assert_eq!(report.root(), fixture.root.join(".loadout"));
        assert!(!report.dry_run());
        assert_eq!(
            fs::read_to_string(fixture.root.join(".loadout/config.yaml")).unwrap(),
            CONFIG_YAML
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join(".loadout/profiles/base.yaml")).unwrap(),
            BASE_YAML
        );
    }

    #[test]
    fn dry_run_creates_nothing() {
        let fixture = Fixture::new();
        let report = initialize(&fixture.root, true).unwrap();

        assert!(report.dry_run());
        assert!(fs::read_dir(&fixture.root).unwrap().next().is_none());
    }

    #[test]
    fn existing_control_entry_is_never_replaced() {
        let fixture = Fixture::new();
        let existing = fixture.root.join(".loadout");
        fs::write(&existing, "user data").unwrap();

        assert!(matches!(
            initialize(&fixture.root, false),
            Err(InitError::AlreadyExists { path }) if path == existing
        ));
        assert_eq!(fs::read_to_string(existing).unwrap(), "user data");
    }

    #[test]
    fn existing_control_directory_is_never_replaced() {
        let fixture = Fixture::new();
        let existing = fixture.root.join(".loadout");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("user-owned"), "preserve me").unwrap();

        assert!(matches!(
            initialize(&fixture.root, false),
            Err(InitError::AlreadyExists { path }) if path == existing
        ));
        assert_eq!(
            fs::read_to_string(existing.join("user-owned")).unwrap(),
            "preserve me"
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_control_symlink_is_never_followed_or_replaced() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let outside = fixture.root.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("user-owned"), "preserve me").unwrap();
        let existing = fixture.root.join(".loadout");
        symlink(&outside, &existing).unwrap();

        assert!(matches!(
            initialize(&fixture.root, false),
            Err(InitError::AlreadyExists { path }) if path == existing
        ));
        assert!(
            fs::symlink_metadata(&existing)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(outside.join("user-owned")).unwrap(),
            "preserve me"
        );
    }

    #[test]
    fn publication_failure_never_exposes_a_partial_control_directory() {
        let fixture = Fixture::new();
        let expected_root = fixture.root.join(".loadout");
        let result = initialize_with(
            &fixture.root,
            false,
            |_, root| {
                Err(InitError::Io {
                    action: "publish control directory",
                    path: root.to_path_buf(),
                    source: io::Error::other("injected publication failure"),
                })
            },
            &mut |_| Ok(()),
        );

        assert!(matches!(result, Err(InitError::Io { .. })));
        assert!(fs::symlink_metadata(&expected_root).is_err());
        assert!(fs::read_dir(&fixture.root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".loadout-init-")
        }));
    }

    #[test]
    fn external_entry_at_publication_is_preserved() {
        let fixture = Fixture::new();
        let expected_root = fixture.root.join(".loadout");
        let result = initialize_with(
            &fixture.root,
            false,
            |_, root| {
                fs::write(root, "external entry").unwrap();
                Err(InitError::AlreadyExists {
                    path: root.to_path_buf(),
                })
            },
            &mut |_| Ok(()),
        );

        assert!(matches!(result, Err(InitError::AlreadyExists { .. })));
        assert_eq!(fs::read_to_string(expected_root).unwrap(), "external entry");
    }

    #[test]
    fn post_publication_substitution_is_not_reported_as_success() {
        let fixture = Fixture::new();
        let expected_root = fixture.root.join(".loadout");
        let result = initialize_with(
            &fixture.root,
            false,
            |staging, root| {
                fs::rename(staging, root).unwrap();
                fs::write(root.join("config.yaml"), "schema_version: 1\n").unwrap();
                Ok(())
            },
            &mut |_| Ok(()),
        );

        assert!(matches!(
            result,
            Err(InitError::InvalidGeneratedBundle { .. })
        ));
        assert!(expected_root.is_dir());
    }

    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    #[test]
    fn no_replace_publication_classifies_a_race_collision_as_already_exists() {
        let fixture = Fixture::new();
        let staging = create_staging_directory(&fixture.root).unwrap();
        build_and_validate(&staging, &mut |_| Ok(())).unwrap();
        let existing = fixture.root.join(".loadout");
        fs::write(&existing, "external entry").unwrap();

        assert!(matches!(
            publish_without_replacement(&staging, &existing),
            Err(InitError::AlreadyExists { path }) if path == existing
        ));
        assert_eq!(fs::read_to_string(existing).unwrap(), "external entry");
        assert!(staging.is_dir());
    }

    #[test]
    fn staging_write_flush_and_validation_failures_never_publish() {
        for fault_point in [
            InitFaultPoint::WriteConfig,
            InitFaultPoint::FlushConfig,
            InitFaultPoint::FlushBaseProfile,
            InitFaultPoint::FlushProfilesDirectory,
            InitFaultPoint::FlushStagingDirectory,
            InitFaultPoint::ValidateStaging,
        ] {
            let fixture = Fixture::new();
            let expected_root = fixture.root.join(".loadout");
            let result = initialize_with(
                &fixture.root,
                false,
                publish_without_replacement,
                &mut |observed| {
                    if observed == fault_point {
                        Err(io::Error::other("injected staging failure"))
                    } else {
                        Ok(())
                    }
                },
            );

            assert!(matches!(result, Err(InitError::Io { .. })));
            assert!(fs::symlink_metadata(&expected_root).is_err());
            assert!(fs::read_dir(&fixture.root).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".loadout-init-")
            }));
        }
    }

    #[cfg(windows)]
    #[test]
    fn existing_windows_symlink_and_junction_are_never_replaced() {
        use std::os::windows::fs::symlink_dir;
        use std::process::Command;

        for entry_kind in ["symlink", "junction"] {
            let fixture = Fixture::new();
            let outside = fixture.root.join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("user-owned"), "preserve me").unwrap();
            let existing = fixture.root.join(".loadout");
            if entry_kind == "symlink" {
                symlink_dir(&outside, &existing).unwrap();
            } else {
                let status = Command::new("cmd")
                    .args(["/C", "mklink", "/J"])
                    .arg(&existing)
                    .arg(&outside)
                    .status()
                    .unwrap();
                assert!(status.success(), "could not create junction");
            }

            assert!(matches!(
                initialize(&fixture.root, false),
                Err(InitError::AlreadyExists { path }) if path == existing
            ));
            assert_eq!(
                fs::read_to_string(outside.join("user-owned")).unwrap(),
                "preserve me"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_no_replace_publication_preserves_an_external_entry() {
        let fixture = Fixture::new();
        let staging = create_staging_directory(&fixture.root).unwrap();
        build_and_validate(&staging, &mut |_| Ok(())).unwrap();
        let existing = fixture.root.join(".loadout");
        fs::write(&existing, "external entry").unwrap();

        assert!(matches!(
            publish_without_replacement(&staging, &existing),
            Err(InitError::AlreadyExists { path }) if path == existing
        ));
        assert_eq!(fs::read_to_string(existing).unwrap(), "external entry");
        assert!(staging.is_dir());
    }
}
