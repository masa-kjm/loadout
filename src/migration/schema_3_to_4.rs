use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use serde_json::Value as JsonValue;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};

use crate::declaration::environment_config::{EnvironmentConfig, EnvironmentConfigError};
use crate::declaration::profile::ProfileDeclaration;
use crate::domain::ids::{ProfileId, ResourceId};
use crate::domain::paths::ResolvedPath;
use crate::filesystem::is_link_or_reparse_point;
use crate::resolver::ResolverContext;
use crate::state::repository::{StateDecodeError, StateRepository, StateRepositoryError};
use crate::state::validate_current_state_bytes;

const LEGACY_SCHEMA_VERSION: u64 = 3;
const CURRENT_SCHEMA_VERSION: u64 = 4;
const MAX_TEMPORARY_NAME_ATTEMPTS: u64 = 128;

static NEXT_PROFILE_TEMPORARY_NONCE: AtomicU64 = AtomicU64::new(0);

/// Test-only publication boundaries for the profile migration protocol.
///
/// These names describe the durable publication sequence rather than an operating-system result.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProfilePublicationFaultPoint {
    CreateTemporary,
    WriteTemporary,
    FlushTemporary,
    ReopenValidation,
    Replace,
}

#[cfg(test)]
thread_local! {
    static PROFILE_PUBLICATION_FAULT: std::cell::RefCell<Option<(ProfilePublicationFaultPoint, usize)>> = const { std::cell::RefCell::new(None) };
}

/// Installs one test-only failure at the selected occurrence of a publication boundary.
#[cfg(test)]
pub(crate) struct ProfilePublicationFaultGuard;

#[cfg(test)]
pub(crate) fn fail_profile_publication_at_for_test(
    point: ProfilePublicationFaultPoint,
    occurrence: usize,
) -> ProfilePublicationFaultGuard {
    assert!(
        occurrence > 0,
        "publication fault occurrence must be nonzero"
    );
    PROFILE_PUBLICATION_FAULT.with_borrow_mut(|slot| {
        assert!(
            slot.is_none(),
            "profile publication fault already installed"
        );
        *slot = Some((point, occurrence));
    });
    ProfilePublicationFaultGuard
}

#[cfg(test)]
impl Drop for ProfilePublicationFaultGuard {
    fn drop(&mut self) {
        PROFILE_PUBLICATION_FAULT.with_borrow_mut(|slot| *slot = None);
    }
}

#[cfg(test)]
fn profile_publication_boundary(point: ProfilePublicationFaultPoint) -> io::Result<()> {
    PROFILE_PUBLICATION_FAULT.with_borrow_mut(|slot| {
        let Some((expected, remaining)) = slot.as_mut() else {
            return Ok(());
        };
        if *expected != point {
            return Ok(());
        }
        *remaining -= 1;
        if *remaining != 0 {
            return Ok(());
        }
        *slot = None;
        Err(io::Error::other(format!(
            "injected profile publication fault at {point:?}"
        )))
    })
}

/// Converts one complete schema-3 profile without inventing parameters or instances.
pub(super) fn convert_profile(source: &str) -> Result<String, MigrationError> {
    validate_legacy_profile_source(source)?;
    let candidate = replace_root_schema_version(source)?;
    ProfileDeclaration::parse(&candidate).map_err(MigrationError::Profile)?;
    Ok(candidate)
}

/// Converts a complete inactive schema-3 state document after preserving its facts byte-for-byte.
pub(super) fn convert_state(source: &[u8]) -> Result<Vec<u8>, MigrationError> {
    let legacy: LegacyStateDocument =
        serde_json::from_slice(source).map_err(MigrationError::State)?;
    if legacy.schema_version != LEGACY_SCHEMA_VERSION {
        return Err(MigrationError::UnsupportedSchemaVersion {
            document: "state",
            actual: legacy.schema_version,
        });
    }
    if legacy.active_operation.is_some() {
        return Err(MigrationError::ActiveOperationPresent);
    }

    let candidate = serde_json::to_vec(&serde_json::json!({
        "schema_version": CURRENT_SCHEMA_VERSION,
        "resources": legacy.resources,
        "active_operation": null,
    }))
    .map_err(MigrationError::State)?;
    validate_current_state_bytes(&candidate).map_err(MigrationError::InvalidConvertedState)?;
    Ok(candidate)
}

/// Performs the state-first barrier under the repository's exclusive migration lock.
/// This facade owns discovery and validation of the complete legacy profile inventory.
/// It snapshots that inventory before locking, then re-discovers, revalidates, and compares it immediately before publication.
pub(super) fn migrate_state(
    repository: &StateRepository,
    context: &ResolverContext,
) -> Result<(), MigrationExecutionError> {
    migrate_state_after_initial_inventory(repository, context, || {})
}

/// Converts each remaining schema-3 profile only after the state barrier is absent or current.
///
/// The state repository's ordinary current-schema reader is intentionally used here: a
/// schema-3 state must fail before any profile temporary is created or profile is replaced.
pub(super) fn migrate_profiles(
    repository: &StateRepository,
    context: &ResolverContext,
) -> Result<(), MigrationExecutionError> {
    repository.load().map_err(MigrationExecutionError::State)?;
    let inventory = discover_profiles_for_profile_migration(context)
        .map_err(MigrationExecutionError::Migration)?;
    for (path, source) in inventory.legacy_documents {
        let candidate = convert_profile(&source).map_err(MigrationExecutionError::Migration)?;
        publish_profile_candidate(&path, &source, &candidate)
            .map_err(MigrationExecutionError::Migration)?;
    }
    Ok(())
}

struct ProfileMigrationInventory {
    legacy_documents: BTreeMap<PathBuf, String>,
}

/// Returns plain paths for the exact schema-3 documents a profiles operation would publish.
pub(super) fn profile_migration_document_paths(
    context: &ResolverContext,
) -> Result<Vec<PathBuf>, MigrationError> {
    Ok(discover_profiles_for_profile_migration(context)?
        .legacy_documents
        .into_keys()
        .collect())
}

/// Validates the complete inventory while allowing the documented mixed state after an
/// interrupted profile migration. Only schema-3 documents are publication candidates.
fn discover_profiles_for_profile_migration(
    context: &ResolverContext,
) -> Result<ProfileMigrationInventory, MigrationError> {
    let config_path = context.environment_config_path().as_ref();
    let config_source =
        fs::read_to_string(config_path).map_err(|source| MigrationError::ConfigRead {
            path: config_path.to_path_buf(),
            source,
        })?;
    let environment = EnvironmentConfig::parse(&config_source).map_err(MigrationError::Config)?;
    let config_directory =
        config_path
            .parent()
            .ok_or(MigrationError::InvalidConfigurationPath {
                value: config_path.display().to_string(),
            })?;
    let mut legacy_documents = BTreeMap::new();
    let mut profile_ids = BTreeMap::<ProfileId, PathBuf>::new();
    let mut includes = Vec::<(ProfileId, ProfileId)>::new();
    for raw_path in environment.profile_discovery_paths() {
        let discovery_path = bind_configuration_path(raw_path, config_directory, context)?;
        let metadata = fs::symlink_metadata(&discovery_path).map_err(|source| {
            MigrationError::ProfileDiscoveryIo {
                path: discovery_path.clone(),
                source,
            }
        })?;
        if is_link_or_reparse_point(&metadata) || !metadata.is_dir() {
            return Err(MigrationError::InvalidProfileDiscoveryPath {
                path: discovery_path,
            });
        }
        for entry in
            fs::read_dir(&discovery_path).map_err(|source| MigrationError::ProfileDiscoveryIo {
                path: discovery_path.clone(),
                source,
            })?
        {
            let path = entry
                .map_err(|source| MigrationError::ProfileDiscoveryIo {
                    path: discovery_path.clone(),
                    source,
                })?
                .path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| {
                MigrationError::ProfileDiscoveryIo {
                    path: path.clone(),
                    source,
                }
            })?;
            if is_link_or_reparse_point(&metadata)
                || !metadata.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("yaml")
            {
                continue;
            }
            let source =
                fs::read_to_string(&path).map_err(|source| MigrationError::ProfileRead {
                    path: path.clone(),
                    source,
                })?;
            let document: YamlValue =
                serde_yaml::from_str(&source).map_err(MigrationError::Profile)?;
            let version = document
                .as_mapping()
                .and_then(|mapping| mapping.get(YamlValue::String("schema_version".to_owned())))
                .and_then(YamlValue::as_u64)
                .ok_or(MigrationError::InvalidLegacyProfile)?;
            match version {
                LEGACY_SCHEMA_VERSION => {
                    let facts = validate_legacy_profile_source(&source)?;
                    if let Some(previous) = profile_ids.insert(facts.id.clone(), path.clone()) {
                        return Err(MigrationError::DuplicateProfileId {
                            id: facts.id.as_str().to_owned(),
                            first: previous,
                            second: path,
                        });
                    }
                    includes.extend(
                        facts
                            .includes
                            .into_iter()
                            .map(|included| (facts.id.clone(), included)),
                    );
                    legacy_documents.insert(path, source);
                }
                CURRENT_SCHEMA_VERSION => {
                    ProfileDeclaration::parse(&source).map_err(MigrationError::Profile)?;
                    let facts = validate_resumable_schema_four_profile_source(&source)?;
                    if let Some(previous) = profile_ids.insert(facts.id.clone(), path.clone()) {
                        return Err(MigrationError::DuplicateProfileId {
                            id: facts.id.as_str().to_owned(),
                            first: previous,
                            second: path,
                        });
                    }
                    includes.extend(
                        facts
                            .includes
                            .into_iter()
                            .map(|included| (facts.id.clone(), included)),
                    );
                }
                actual => {
                    return Err(MigrationError::UnsupportedSchemaVersion {
                        document: "profile",
                        actual,
                    });
                }
            }
        }
    }
    validate_legacy_profile_composition(&profile_ids, &includes)?;
    Ok(ProfileMigrationInventory { legacy_documents })
}

fn publish_profile_candidate(
    path: &Path,
    source: &str,
    candidate: &str,
) -> Result<(), MigrationError> {
    let parent = path
        .parent()
        .ok_or(MigrationError::InvalidConfigurationPath {
            value: path.display().to_string(),
        })?;
    verify_profile_parent(parent)?;
    verify_profile_source(path, source)?;
    let temporary_path = profile_temporary_path(parent)?;
    let result = (|| {
        #[cfg(test)]
        profile_publication_boundary(ProfilePublicationFaultPoint::CreateTemporary).map_err(
            |source| MigrationError::ProfilePublication {
                path: temporary_path.clone(),
                source,
            },
        )?;
        let mut temporary = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .map_err(|source| MigrationError::ProfilePublication {
                path: temporary_path.clone(),
                source,
            })?;
        #[cfg(test)]
        profile_publication_boundary(ProfilePublicationFaultPoint::WriteTemporary).map_err(
            |source| MigrationError::ProfilePublication {
                path: temporary_path.clone(),
                source,
            },
        )?;
        temporary
            .write_all(candidate.as_bytes())
            .map_err(|source| MigrationError::ProfilePublication {
                path: temporary_path.clone(),
                source,
            })?;
        #[cfg(test)]
        profile_publication_boundary(ProfilePublicationFaultPoint::FlushTemporary).map_err(
            |source| MigrationError::ProfilePublication {
                path: temporary_path.clone(),
                source,
            },
        )?;
        temporary
            .sync_all()
            .map_err(|source| MigrationError::ProfilePublication {
                path: temporary_path.clone(),
                source,
            })?;
        drop(temporary);
        #[cfg(test)]
        profile_publication_boundary(ProfilePublicationFaultPoint::ReopenValidation).map_err(
            |source| MigrationError::ProfilePublication {
                path: temporary_path.clone(),
                source,
            },
        )?;
        verify_profile_temporary(&temporary_path, candidate)?;
        verify_profile_parent(parent)?;
        verify_profile_source(path, source)?;
        verify_profile_temporary(&temporary_path, candidate)?;
        #[cfg(test)]
        profile_publication_boundary(ProfilePublicationFaultPoint::Replace).map_err(|source| {
            MigrationError::ProfilePublication {
                path: path.to_path_buf(),
                source,
            }
        })?;
        fs::rename(&temporary_path, path).map_err(|source| MigrationError::ProfilePublication {
            path: path.to_path_buf(),
            source,
        })?;
        verify_profile_parent(parent)?;
        verify_profile_temporary(path, candidate)?;
        sync_profile_parent(parent)
    })();
    if result.is_err() {
        let _ = remove_profile_temporary_if_expected(&temporary_path, candidate);
    }
    result
}

#[cfg(unix)]
fn sync_profile_parent(parent: &Path) -> Result<(), MigrationError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| MigrationError::ProfilePublication {
            path: parent.to_path_buf(),
            source,
        })
}

#[cfg(windows)]
fn sync_profile_parent(_parent: &Path) -> Result<(), MigrationError> {
    Ok(())
}

fn verify_profile_parent(parent: &Path) -> Result<(), MigrationError> {
    if !parent.is_absolute() {
        return Err(MigrationError::InvalidConfigurationPath {
            value: parent.display().to_string(),
        });
    }
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        if !current.is_absolute() {
            continue;
        }
        let metadata = fs::symlink_metadata(&current).map_err(|source| {
            MigrationError::ProfilePublication {
                path: current.clone(),
                source,
            }
        })?;
        if !metadata.is_dir() || is_link_or_reparse_point(&metadata) {
            return Err(MigrationError::InvalidProfilePublicationPath { path: current });
        }
        let declared = ResolvedPath::from_platform_existing(current.clone()).map_err(|_| {
            MigrationError::InvalidProfilePublicationPath {
                path: current.clone(),
            }
        })?;
        let observed = ResolvedPath::from_platform_canonicalized(
            fs::canonicalize(&current).map_err(|source| MigrationError::ProfilePublication {
                path: current.clone(),
                source,
            })?,
        )
        .map_err(|_| MigrationError::InvalidProfilePublicationPath {
            path: current.clone(),
        })?;
        if declared != observed {
            return Err(MigrationError::InvalidProfilePublicationPath { path: current });
        }
    }
    Ok(())
}

fn verify_profile_source(path: &Path, expected: &str) -> Result<(), MigrationError> {
    verify_profile_regular_file(path)?;
    let observed =
        fs::read_to_string(path).map_err(|source| MigrationError::ProfilePublication {
            path: path.to_path_buf(),
            source,
        })?;
    if observed == expected {
        Ok(())
    } else {
        Err(MigrationError::ProfileSourceChanged {
            path: path.to_path_buf(),
        })
    }
}

fn verify_profile_temporary(path: &Path, expected: &str) -> Result<(), MigrationError> {
    verify_profile_regular_file(path)?;
    let written =
        fs::read_to_string(path).map_err(|source| MigrationError::ProfilePublication {
            path: path.to_path_buf(),
            source,
        })?;
    if written != expected {
        return Err(MigrationError::ProfileTemporaryChanged {
            path: path.to_path_buf(),
        });
    }
    ProfileDeclaration::parse(&written)
        .map(|_| ())
        .map_err(MigrationError::Profile)
}

fn verify_profile_regular_file(path: &Path) -> Result<(), MigrationError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| MigrationError::ProfilePublication {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.is_file() && !is_link_or_reparse_point(&metadata) {
        Ok(())
    } else {
        Err(MigrationError::InvalidProfilePublicationPath {
            path: path.to_path_buf(),
        })
    }
}

fn remove_profile_temporary_if_expected(path: &Path, expected: &str) -> Result<(), MigrationError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    verify_profile_parent(parent)?;
    match verify_profile_temporary(path, expected) {
        Ok(()) => fs::remove_file(path).map_err(|source| MigrationError::ProfilePublication {
            path: path.to_path_buf(),
            source,
        }),
        Err(MigrationError::ProfilePublication { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn profile_temporary_path(parent: &Path) -> Result<PathBuf, MigrationError> {
    for _ in 0..MAX_TEMPORARY_NAME_ATTEMPTS {
        let nonce = NEXT_PROFILE_TEMPORARY_NONCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".loadout-profile-migrate-{}-{nonce}.tmp",
            std::process::id()
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(MigrationError::ProfileTemporaryNameExhausted {
        path: parent.to_path_buf(),
    })
}

/// Keeps the recheck seam private to migration so tests can model a profile edit while the state lock is pending.
fn migrate_state_after_initial_inventory(
    repository: &StateRepository,
    context: &ResolverContext,
    before_locked_recheck: impl FnOnce(),
) -> Result<(), MigrationExecutionError> {
    // Validate the state before lock acquisition so an invalid legacy document cannot create
    // a migration lock as a side effect. It is read and validated again while locked because
    // this preliminary observation cannot authorize publication.
    let initial_state_source = repository
        .read_source_for_migration()
        .map_err(MigrationExecutionError::State)?;
    convert_state(&initial_state_source).map_err(MigrationExecutionError::Migration)?;
    let inventory =
        discover_legacy_profile_inventory(context).map_err(MigrationExecutionError::Migration)?;
    before_locked_recheck();
    let locked = repository
        .acquire_exclusive_for_migration()
        .map_err(MigrationExecutionError::State)?;
    let rechecked_inventory =
        discover_legacy_profile_inventory(context).map_err(MigrationExecutionError::Migration)?;
    if inventory != rechecked_inventory {
        return Err(MigrationExecutionError::Migration(
            MigrationError::ProfileInventoryChanged,
        ));
    }
    let source = locked
        .read_source()
        .map_err(MigrationExecutionError::State)?;
    let candidate = convert_state(&source).map_err(MigrationExecutionError::Migration)?;
    locked
        .publish_current(&candidate)
        .map_err(MigrationExecutionError::State)
}

/// A validated, byte-exact discovery snapshot used solely to detect a profile replacement before the state barrier is published.
#[derive(Debug, Eq, PartialEq)]
struct LegacyProfileInventory {
    documents: BTreeMap<PathBuf, String>,
}

/// Discovers every configured profile exactly as the migration boundary will validate it.
/// No caller-provided predicate can weaken this inventory check.
fn discover_legacy_profile_inventory(
    context: &ResolverContext,
) -> Result<LegacyProfileInventory, MigrationError> {
    let config_path = context.environment_config_path().as_ref();
    let config_source =
        fs::read_to_string(config_path).map_err(|source| MigrationError::ConfigRead {
            path: config_path.to_path_buf(),
            source,
        })?;
    let environment = EnvironmentConfig::parse(&config_source).map_err(MigrationError::Config)?;
    let config_directory =
        config_path
            .parent()
            .ok_or(MigrationError::InvalidConfigurationPath {
                value: config_path.display().to_string(),
            })?;

    let mut documents = BTreeMap::new();
    let mut profile_ids = BTreeMap::<ProfileId, PathBuf>::new();
    let mut includes = Vec::<(ProfileId, ProfileId)>::new();
    for raw_path in environment.profile_discovery_paths() {
        let discovery_path = bind_configuration_path(raw_path, config_directory, context)?;
        let metadata = fs::symlink_metadata(&discovery_path).map_err(|source| {
            MigrationError::ProfileDiscoveryIo {
                path: discovery_path.clone(),
                source,
            }
        })?;
        if is_link_or_reparse_point(&metadata) || !metadata.is_dir() {
            return Err(MigrationError::InvalidProfileDiscoveryPath {
                path: discovery_path,
            });
        }
        let physical_path = fs::canonicalize(&discovery_path).map_err(|source| {
            MigrationError::ProfileDiscoveryIo {
                path: discovery_path.clone(),
                source,
            }
        })?;
        for entry in
            fs::read_dir(&physical_path).map_err(|source| MigrationError::ProfileDiscoveryIo {
                path: physical_path.clone(),
                source,
            })?
        {
            let entry = entry.map_err(|source| MigrationError::ProfileDiscoveryIo {
                path: physical_path.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| {
                MigrationError::ProfileDiscoveryIo {
                    path: path.clone(),
                    source,
                }
            })?;
            if is_link_or_reparse_point(&metadata)
                || !metadata.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("yaml")
            {
                continue;
            }
            let source =
                fs::read_to_string(&path).map_err(|source| MigrationError::ProfileRead {
                    path: path.clone(),
                    source,
                })?;
            let profile = validate_legacy_profile_source(&source)?;
            if let Some(previous) = profile_ids.insert(profile.id.clone(), path.clone()) {
                return Err(MigrationError::DuplicateProfileId {
                    id: profile.id.as_str().to_owned(),
                    first: previous,
                    second: path,
                });
            }
            let profile_id = profile.id;
            includes.extend(
                profile
                    .includes
                    .into_iter()
                    .map(|included| (profile_id.clone(), included)),
            );
            documents.insert(path, source);
        }
    }
    validate_legacy_profile_composition(&profile_ids, &includes)?;
    Ok(LegacyProfileInventory { documents })
}

fn validate_legacy_profile_composition(
    profile_ids: &BTreeMap<ProfileId, PathBuf>,
    includes: &[(ProfileId, ProfileId)],
) -> Result<(), MigrationError> {
    let mut adjacency = BTreeMap::<ProfileId, Vec<ProfileId>>::new();
    for profile_id in profile_ids.keys() {
        adjacency.insert(profile_id.clone(), Vec::new());
    }
    for (including, included) in includes {
        if !profile_ids.contains_key(included) {
            return Err(MigrationError::UndiscoveredIncludedProfile {
                including: including.as_str().to_owned(),
                included: included.as_str().to_owned(),
            });
        }
        adjacency
            .get_mut(including)
            .expect("including profile was discovered")
            .push(included.clone());
    }
    fn visit(
        profile: &ProfileId,
        adjacency: &BTreeMap<ProfileId, Vec<ProfileId>>,
        visiting: &mut Vec<ProfileId>,
        visited: &mut BTreeSet<ProfileId>,
    ) -> Result<(), MigrationError> {
        if visited.contains(profile) {
            return Ok(());
        }
        if let Some(start) = visiting.iter().position(|candidate| candidate == profile) {
            let mut cycle = visiting[start..]
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>();
            cycle.push(profile.as_str().to_owned());
            return Err(MigrationError::IncludeCycle { cycle });
        }
        visiting.push(profile.clone());
        for included in &adjacency[profile] {
            visit(included, adjacency, visiting, visited)?;
        }
        visiting.pop();
        visited.insert(profile.clone());
        Ok(())
    }
    let mut visiting = Vec::new();
    let mut visited = BTreeSet::new();
    for profile in profile_ids.keys() {
        visit(profile, &adjacency, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn bind_configuration_path(
    raw_path: &str,
    config_directory: &Path,
    context: &ResolverContext,
) -> Result<PathBuf, MigrationError> {
    if raw_path == "~" || (raw_path.starts_with('~') && !raw_path.starts_with("~/")) {
        return Err(MigrationError::InvalidConfigurationPath {
            value: raw_path.to_owned(),
        });
    }
    if let Some(relative_to_home) = raw_path.strip_prefix("~/") {
        return Ok(context.home_directory().as_ref().join(relative_to_home));
    }
    let path = Path::new(raw_path);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_directory.join(path)
    })
}

struct LegacyProfileFacts {
    id: ProfileId,
    includes: Vec<ProfileId>,
}

fn validate_legacy_profile_source(source: &str) -> Result<LegacyProfileFacts, MigrationError> {
    let document: YamlValue = serde_yaml::from_str(source).map_err(MigrationError::Profile)?;
    validate_legacy_profile(&document)?;
    let mapping = document
        .as_mapping()
        .expect("legacy profile validation requires a mapping");
    let id = ProfileId::parse(require_yaml_string(mapping, "id")?.to_owned())
        .map_err(|_| MigrationError::InvalidLegacyProfile)?;
    let resources = mapping
        .get(YamlValue::String("resources".to_owned()))
        .and_then(YamlValue::as_mapping)
        .expect("legacy profile validation requires resources");
    for resource_id in resources.keys() {
        ResourceId::parse(
            resource_id
                .as_str()
                .expect("legacy validation requires string resource IDs")
                .to_owned(),
        )
        .map_err(|_| MigrationError::InvalidLegacyProfile)?;
    }
    let no_includes = Vec::new();
    let includes = match mapping.get(YamlValue::String("includes".to_owned())) {
        Some(value) => value
            .as_sequence()
            .ok_or(MigrationError::InvalidLegacyProfile)?,
        None => &no_includes,
    }
    .iter()
    .map(|include| {
        let include = include
            .as_mapping()
            .ok_or(MigrationError::InvalidLegacyProfile)?;
        ProfileId::parse(require_yaml_string(include, "id")?.to_owned())
            .map_err(|_| MigrationError::InvalidLegacyProfile)
    })
    .collect::<Result<Vec<_>, _>>()?;
    Ok(LegacyProfileFacts { id, includes })
}

fn validate_resumable_schema_four_profile_source(
    source: &str,
) -> Result<LegacyProfileFacts, MigrationError> {
    let mut document: YamlValue = serde_yaml::from_str(source).map_err(MigrationError::Profile)?;
    let mapping = document
        .as_mapping_mut()
        .ok_or(MigrationError::InvalidLegacyProfile)?;
    mapping.insert(
        YamlValue::String("schema_version".to_owned()),
        YamlValue::Number(LEGACY_SCHEMA_VERSION.into()),
    );
    validate_legacy_profile_source_from_value(&document)
}

fn validate_legacy_profile_source_from_value(
    document: &YamlValue,
) -> Result<LegacyProfileFacts, MigrationError> {
    validate_legacy_profile(document)?;
    let mapping = document
        .as_mapping()
        .expect("legacy profile validation requires a mapping");
    let id = ProfileId::parse(require_yaml_string(mapping, "id")?.to_owned())
        .map_err(|_| MigrationError::InvalidLegacyProfile)?;
    let resources = mapping
        .get(YamlValue::String("resources".to_owned()))
        .and_then(YamlValue::as_mapping)
        .expect("legacy profile validation requires resources");
    for resource_id in resources.keys() {
        ResourceId::parse(
            resource_id
                .as_str()
                .expect("legacy validation requires string resource IDs")
                .to_owned(),
        )
        .map_err(|_| MigrationError::InvalidLegacyProfile)?;
    }
    let no_includes = Vec::new();
    let includes = match mapping.get(YamlValue::String("includes".to_owned())) {
        Some(value) => value
            .as_sequence()
            .ok_or(MigrationError::InvalidLegacyProfile)?,
        None => &no_includes,
    }
    .iter()
    .map(|include| {
        let include = include
            .as_mapping()
            .ok_or(MigrationError::InvalidLegacyProfile)?;
        ProfileId::parse(require_yaml_string(include, "id")?.to_owned())
            .map_err(|_| MigrationError::InvalidLegacyProfile)
    })
    .collect::<Result<Vec<_>, _>>()?;
    Ok(LegacyProfileFacts { id, includes })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyStateDocument {
    schema_version: u64,
    resources: BTreeMap<String, LegacyKnownResource>,
    active_operation: Option<JsonValue>,
}

/// The schema-3 state surface that migration can preserve without interpretation.
///
/// These DTOs are deliberately strict even though conversion retains their values verbatim:
/// accepting an unknown nested field would make a later schema-4 publication silently carry
/// an unreviewed legacy fact across the schema boundary.
#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyKnownResource {
    definition_hash: String,
    effect: LegacyKnownEffect,
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum LegacyKnownEffect {
    FileLink {
        source_path: String,
        target_path: String,
        link_target: String,
    },
    FileCopy {
        source_path: String,
        target_path: String,
        content_fingerprint: String,
    },
    DirectoryLink {
        source_path: String,
        target_path: String,
        link_target: String,
    },
}

fn validate_legacy_profile(document: &YamlValue) -> Result<(), MigrationError> {
    let mapping = document
        .as_mapping()
        .ok_or(MigrationError::InvalidLegacyProfile)?;
    require_yaml_keys(mapping, &["schema_version", "id", "includes", "resources"])?;
    let version = mapping
        .get(YamlValue::String("schema_version".to_owned()))
        .and_then(YamlValue::as_u64)
        .ok_or(MigrationError::InvalidLegacyProfile)?;
    if version != LEGACY_SCHEMA_VERSION {
        return Err(MigrationError::UnsupportedSchemaVersion {
            document: "profile",
            actual: version,
        });
    }
    require_yaml_string(mapping, "id")?;
    let resources = mapping
        .get(YamlValue::String("resources".to_owned()))
        .and_then(YamlValue::as_mapping)
        .ok_or(MigrationError::InvalidLegacyProfile)?;
    for (resource_id, resource) in resources {
        if resource_id.as_str().is_none() {
            return Err(MigrationError::InvalidLegacyProfile);
        }
        validate_legacy_resource(resource)?;
    }
    if let Some(includes) = mapping.get(YamlValue::String("includes".to_owned())) {
        for include in includes
            .as_sequence()
            .ok_or(MigrationError::InvalidLegacyProfile)?
        {
            let include = include
                .as_mapping()
                .ok_or(MigrationError::InvalidLegacyProfile)?;
            require_yaml_keys(include, &["id"])?;
            require_yaml_string(include, "id")?;
        }
    }
    Ok(())
}

fn validate_legacy_resource(value: &YamlValue) -> Result<(), MigrationError> {
    let resource = value
        .as_mapping()
        .ok_or(MigrationError::InvalidLegacyProfile)?;
    require_yaml_keys(resource, &["type", "properties"])?;
    if require_yaml_string(resource, "type")? != "file" {
        return Err(MigrationError::InvalidLegacyProfile);
    }
    let properties = resource
        .get(YamlValue::String("properties".to_owned()))
        .and_then(YamlValue::as_mapping)
        .ok_or(MigrationError::InvalidLegacyProfile)?;
    require_yaml_keys(properties, &["kind", "source", "target", "operation"])?;
    let kind = require_yaml_string(properties, "kind")?;
    let operation = require_yaml_string(properties, "operation")?;
    require_yaml_string(properties, "target")?;
    if !matches!(kind, "file" | "directory")
        || !matches!(operation, "link" | "copy")
        || (kind == "directory" && operation == "copy")
    {
        return Err(MigrationError::InvalidLegacyProfile);
    }
    let source = properties
        .get(YamlValue::String("source".to_owned()))
        .and_then(YamlValue::as_mapping)
        .ok_or(MigrationError::InvalidLegacyProfile)?;
    require_yaml_keys(source, &["store", "path"])?;
    require_yaml_string(source, "store")?;
    require_yaml_string(source, "path")?;
    Ok(())
}

fn require_yaml_keys(mapping: &YamlMapping, allowed: &[&str]) -> Result<(), MigrationError> {
    let allowed = allowed.iter().copied().collect::<BTreeSet<_>>();
    if mapping
        .keys()
        .any(|key| key.as_str().is_none_or(|key| !allowed.contains(key)))
    {
        return Err(MigrationError::InvalidLegacyProfile);
    }
    Ok(())
}

fn require_yaml_string<'a>(mapping: &'a YamlMapping, key: &str) -> Result<&'a str, MigrationError> {
    mapping
        .get(YamlValue::String(key.to_owned()))
        .and_then(YamlValue::as_str)
        .ok_or(MigrationError::InvalidLegacyProfile)
}

/// Replaces the required root schema value without reserializing the authoring document.
///
/// Profile migration must not normalize comments, mapping order, quoting, or other authoring
/// representation because the schema version is the only permitted profile change.
fn replace_root_schema_version(source: &str) -> Result<String, MigrationError> {
    let mut result = String::with_capacity(source.len());
    let mut replaced = false;

    for line in source.split_inclusive('\n') {
        if !replaced {
            if let Some(value_start) = root_schema_version_value_start(line)
                .or_else(|| flow_root_schema_version_value_start(line))
            {
                let value = &line[value_start..];
                if let Some(suffix) = value.strip_prefix('3') {
                    if suffix.is_empty()
                        || suffix.chars().next().is_some_and(char::is_whitespace)
                        || suffix.starts_with('#')
                        || suffix.starts_with(',')
                        || suffix.starts_with('}')
                    {
                        result.push_str(&line[..value_start]);
                        result.push('4');
                        result.push_str(suffix);
                        replaced = true;
                        continue;
                    }
                }
            }
        }
        result.push_str(line);
    }

    if !replaced {
        return Err(MigrationError::InvalidLegacyProfile);
    }
    Ok(result)
}

/// Returns the value offset for the supported top-level YAML spellings of `schema_version`.
/// The surrounding text is retained exactly so profile migration changes only the numeric value.
fn root_schema_version_value_start(line: &str) -> Option<usize> {
    let without_newline = line.strip_suffix('\n').unwrap_or(line);
    let leading = without_newline.len() - without_newline.trim_start_matches([' ', '\t']).len();
    let rest = &without_newline[leading..];
    let key_length = if rest.starts_with("schema_version") {
        "schema_version".len()
    } else if rest.starts_with("\"schema_version\"") {
        "\"schema_version\"".len()
    } else if rest.starts_with("'schema_version'") {
        "'schema_version'".len()
    } else {
        return None;
    };
    let after_key = &rest[key_length..];
    let spaces = after_key.len() - after_key.trim_start_matches([' ', '\t']).len();
    let after_spaces = &after_key[spaces..];
    if !after_spaces.starts_with(':') {
        return None;
    }
    let after_colon = &after_spaces[1..];
    let value_spaces = after_colon.len() - after_colon.trim_start_matches([' ', '\t']).len();
    Some(leading + key_length + spaces + 1 + value_spaces)
}

fn flow_root_schema_version_value_start(line: &str) -> Option<usize> {
    let mut offset = 0;
    let bytes = line.as_bytes();
    while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
        offset += 1;
    }
    if bytes.get(offset) != Some(&b'{') {
        return None;
    }
    offset += 1;
    loop {
        while bytes
            .get(offset)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b',')
        {
            offset += 1;
        }
        if bytes.get(offset) == Some(&b'}') || offset >= bytes.len() {
            return None;
        }
        let key_start = offset;
        let key_end = if matches!(bytes.get(offset), Some(b'\'' | b'"')) {
            let quote = bytes[offset];
            offset += 1;
            let content_start = offset;
            while bytes.get(offset) != Some(&quote) {
                offset += 1;
            }
            let content_end = offset;
            offset += 1;
            if &line[content_start..content_end] == "schema_version" {
                content_end
            } else {
                key_start
            }
        } else {
            while bytes
                .get(offset)
                .is_some_and(|byte| *byte != b':' && !byte.is_ascii_whitespace())
            {
                offset += 1;
            }
            offset
        };
        let key_is_schema = if key_end == key_start {
            false
        } else if matches!(bytes.get(key_start), Some(b'\'' | b'"')) {
            let quote = bytes[key_start];
            line[key_start + 1..]
                .find(quote as char)
                .is_some_and(|length| {
                    &line[key_start + 1..key_start + 1 + length] == "schema_version"
                })
        } else {
            &line[key_start..key_end] == "schema_version"
        };
        while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset += 1;
        }
        if bytes.get(offset) != Some(&b':') {
            return None;
        }
        offset += 1;
        while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset += 1;
        }
        if key_is_schema {
            return Some(offset);
        }
        let mut nested = 0_u32;
        let mut quote = None;
        while let Some(byte) = bytes.get(offset) {
            if let Some(expected) = quote {
                if *byte == expected {
                    quote = None;
                }
            } else {
                match byte {
                    b'\'' | b'"' => quote = Some(*byte),
                    b'{' | b'[' => nested += 1,
                    b'}' | b']' if nested > 0 => nested -= 1,
                    b',' if nested == 0 => break,
                    b'}' if nested == 0 => return None,
                    _ => {}
                }
            }
            offset += 1;
        }
    }
}

/// A migration-only failure that leaves original documents authoritative.
#[derive(Debug)]
pub(crate) enum MigrationError {
    Profile(serde_yaml::Error),
    State(serde_json::Error),
    Config(EnvironmentConfigError),
    ConfigRead {
        path: PathBuf,
        source: io::Error,
    },
    InvalidConfigurationPath {
        value: String,
    },
    ProfileDiscoveryIo {
        path: PathBuf,
        source: io::Error,
    },
    InvalidProfileDiscoveryPath {
        path: PathBuf,
    },
    ProfileRead {
        path: PathBuf,
        source: io::Error,
    },
    DuplicateProfileId {
        id: String,
        first: PathBuf,
        second: PathBuf,
    },
    UndiscoveredIncludedProfile {
        including: String,
        included: String,
    },
    ProfileInventoryChanged,
    ProfileTemporaryNameExhausted {
        path: PathBuf,
    },
    InvalidProfilePublicationPath {
        path: PathBuf,
    },
    ProfileSourceChanged {
        path: PathBuf,
    },
    ProfileTemporaryChanged {
        path: PathBuf,
    },
    IncludeCycle {
        cycle: Vec<String>,
    },
    ProfilePublication {
        path: PathBuf,
        source: io::Error,
    },
    UnsupportedSchemaVersion {
        document: &'static str,
        actual: u64,
    },
    InvalidLegacyProfile,
    ActiveOperationPresent,
    InvalidConvertedState(StateDecodeError),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(error) => {
                write!(formatter, "invalid profile migration document: {error}")
            }
            Self::State(error) => {
                write!(formatter, "invalid state migration document: {error}")
            }
            Self::Config(error) => {
                write!(formatter, "invalid migration configuration: {error}")
            }
            Self::ConfigRead { path, source } => {
                write!(
                    formatter,
                    "cannot read migration configuration {}: {source}",
                    path.display()
                )
            }
            Self::InvalidConfigurationPath { value } => {
                write!(formatter, "invalid profile discovery path {value:?}")
            }
            Self::ProfileDiscoveryIo { path, source } => {
                write!(
                    formatter,
                    "cannot discover profiles in {}: {source}",
                    path.display()
                )
            }
            Self::InvalidProfileDiscoveryPath { path } => {
                write!(
                    formatter,
                    "profile discovery path is not a plain directory: {}",
                    path.display()
                )
            }
            Self::ProfileRead { path, source } => {
                write!(
                    formatter,
                    "cannot read profile {}: {source}",
                    path.display()
                )
            }
            Self::DuplicateProfileId { id, first, second } => write!(
                formatter,
                "duplicate discovered profile ID {id:?} in {} and {}",
                first.display(),
                second.display()
            ),
            Self::UndiscoveredIncludedProfile {
                including,
                included,
            } => write!(
                formatter,
                "profile {including:?} includes undiscovered profile {included:?}"
            ),
            Self::ProfileInventoryChanged => formatter
                .write_str("profile inventory changed while state migration waited for its lock"),
            Self::ProfileTemporaryNameExhausted { path } => write!(
                formatter,
                "could not allocate a unique profile migration temporary in {}",
                path.display()
            ),
            Self::InvalidProfilePublicationPath { path } => write!(
                formatter,
                "unsafe profile publication path: {}",
                path.display()
            ),
            Self::ProfileSourceChanged { path } => write!(
                formatter,
                "profile changed before publication: {}",
                path.display()
            ),
            Self::ProfileTemporaryChanged { path } => write!(
                formatter,
                "profile migration temporary changed before publication: {}",
                path.display()
            ),
            Self::IncludeCycle { cycle } => {
                write!(formatter, "profile include cycle: {cycle:?}")
            }
            Self::ProfilePublication { path, source } => write!(
                formatter,
                "cannot publish migrated profile {}: {source}",
                path.display()
            ),
            Self::UnsupportedSchemaVersion { document, actual } => write!(
                formatter,
                "unsupported {document} schema_version {actual}; expected {LEGACY_SCHEMA_VERSION}"
            ),
            Self::InvalidLegacyProfile => {
                formatter.write_str("profile is not a complete schema-3 declaration")
            }
            Self::ActiveOperationPresent => {
                formatter.write_str("state migration requires no active operation")
            }
            Self::InvalidConvertedState(error) => write!(
                formatter,
                "schema-3 state is not losslessly convertible: {error}"
            ),
        }
    }
}

impl std::error::Error for MigrationError {}

/// Failure while executing the narrow locked state-migration facade.
#[derive(Debug)]
pub(crate) enum MigrationExecutionError {
    Migration(MigrationError),
    State(StateRepositoryError),
}

impl fmt::Display for MigrationExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Migration(error) => error.fmt(formatter),
            Self::State(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MigrationExecutionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::domain::paths::ResolvedPath;

    const PROFILE: &str = "schema_version: 3\nid: base\nresources:\n  git-config:\n    type: file\n    properties:\n      kind: file\n      source: { store: dotfiles, path: git/config }\n      target: /home/example/.gitconfig\n      operation: link\n";
    #[cfg(unix)]
    const STATE: &str = r#"{"schema_version":3,"resources":{"base/git-config":{"definition_hash":"sha256:1250e6bf4abdb529664c1444709aeae2ce814e8b97c3984e32d46d9c971b3a5e","effect":{"kind":"file_link","source_path":"/home/example/dotfiles/git/config","target_path":"/home/example/.gitconfig","link_target":"/home/example/dotfiles/git/config"}}},"active_operation":null}"#;
    #[cfg(windows)]
    const STATE: &str = r#"{"schema_version":3,"resources":{"base/git-config":{"definition_hash":"sha256:553a59aab51ec70ffcec1e6c4dd3ebb4059bb3a257cd2cc3ccd5ef5908849348","effect":{"kind":"file_link","source_path":"C:\\loadout\\dotfiles\\git\\config","target_path":"C:\\loadout\\.gitconfig","link_target":"C:\\loadout\\dotfiles\\git\\config"}}},"active_operation":null}"#;

    #[test]
    fn profile_conversion_only_admits_complete_schema_three_documents() {
        let converted = convert_profile(PROFILE).unwrap();
        assert!(converted.contains("schema_version: 4"));
        assert_eq!(
            converted,
            PROFILE.replacen("schema_version: 3", "schema_version: 4", 1)
        );
        assert!(ProfileDeclaration::parse(&converted).is_ok());
        assert!(
            convert_profile(&PROFILE.replace("schema_version: 3", "schema_version: 2")).is_err()
        );
        assert!(convert_profile(&PROFILE.replace("id: base", "parameters: {}\nid: base")).is_err());
    }

    #[test]
    fn profile_conversion_rejects_unsupported_legacy_resource_effects() {
        for (label, profile) in [
            (
                "unsupported-kind",
                PROFILE.replace("kind: file", "kind: device"),
            ),
            (
                "unsupported-operation",
                PROFILE.replace("operation: link", "operation: synchronize"),
            ),
            (
                "directory-copy",
                PROFILE
                    .replace("kind: file", "kind: directory")
                    .replace("operation: link", "operation: copy"),
            ),
        ] {
            assert!(convert_profile(&profile).is_err(), "{label} must reject");
        }
    }

    #[test]
    fn profile_conversion_preserves_comments_and_mapping_order() {
        let source =
            "# kept\nid: base\nresources: {}\nschema_version: 3 # only this value changes\n";

        assert_eq!(
            convert_profile(source).unwrap(),
            "# kept\nid: base\nresources: {}\nschema_version: 4 # only this value changes\n"
        );
    }

    #[test]
    fn profile_conversion_preserves_quoted_key_and_pre_colon_whitespace() {
        for source in [
            PROFILE.replacen("schema_version: 3", "\"schema_version\": 3", 1),
            PROFILE.replacen("schema_version: 3", "schema_version \t: 3", 1),
            PROFILE.replacen("schema_version: 3", "'schema_version' : 3 # kept", 1),
        ] {
            let converted = convert_profile(&source).unwrap();
            assert_eq!(converted.replace('4', "3"), source);
            assert!(ProfileDeclaration::parse(&converted).is_ok());
        }
    }

    #[test]
    fn profile_conversion_supports_a_valid_flow_style_root_mapping() {
        let source = "{ schema_version: 3, id: base, resources: {} }\n";
        assert_eq!(
            convert_profile(source).unwrap(),
            "{ schema_version: 4, id: base, resources: {} }\n"
        );
    }

    #[test]
    fn schema_four_resume_requires_observable_legacy_equivalence() {
        let resumable = PROFILE.replace("schema_version: 3", "schema_version: 4");
        assert!(validate_resumable_schema_four_profile_source(&resumable).is_ok());
        assert!(
            validate_resumable_schema_four_profile_source(
                &resumable.replace("id: base", "parameters: { theme: string }\nid: base")
            )
            .is_err()
        );
        assert!(
            validate_resumable_schema_four_profile_source(&resumable.replace(
                "resources:",
                "includes: [{ id: base, as: alias }]\nresources:"
            ))
            .is_err()
        );
    }

    #[test]
    fn state_conversion_preserves_resource_facts_and_rejects_active_operations() {
        let source = STATE.to_owned();
        let converted = convert_state(source.as_bytes()).unwrap();
        let value: JsonValue = serde_json::from_slice(&converted).unwrap();
        let original: JsonValue = serde_json::from_str(&source).unwrap();
        assert_eq!(value["schema_version"], CURRENT_SCHEMA_VERSION);
        assert_eq!(value["resources"], original["resources"]);
        assert_eq!(
            value["resources"]["base/git-config"]["definition_hash"],
            original["resources"]["base/git-config"]["definition_hash"]
        );
        assert!(matches!(
            convert_state(source.replace("null", "{}").as_bytes()),
            Err(MigrationError::ActiveOperationPresent)
        ));
    }

    #[test]
    fn state_conversion_rejects_unknown_legacy_fields_at_every_retained_level() {
        for (label, source) in [
            (
                "root",
                STATE.replace("\"resources\":", "\"unexpected\":true,\"resources\":"),
            ),
            (
                "resource",
                STATE.replace("\"effect\":{", "\"unexpected\":true,\"effect\":{"),
            ),
            (
                "effect",
                STATE.replace(
                    "\"kind\":\"file_link\",",
                    "\"kind\":\"file_link\",\"unexpected\":true,",
                ),
            ),
        ] {
            assert!(
                convert_state(source.as_bytes()).is_err(),
                "{label} must reject"
            );
        }
    }

    #[test]
    fn state_migration_facade_owns_inventory_rejection_without_writing_state() {
        let invalid = "schema_version: 3\nid: base\nresources: []\n";
        let mixed = PROFILE.replace("schema_version: 3", "schema_version: 4");
        for (label, profile) in [("invalid", invalid), ("mixed", mixed.as_str())] {
            let fixture_root = migration_fixture(label, profile, true);
            let before = fs::read(fixture_root.root.join("state/state.json")).unwrap();
            let result = migrate_state(&fixture_root.repository, &fixture_root.context);
            assert!(result.is_err(), "{label} inventory must reject");
            assert_eq!(
                fs::read(fixture_root.root.join("state/state.json")).unwrap(),
                before
            );
            fixture_root.remove();
        }

        let fixture_root = migration_fixture("missing", PROFILE, false);
        let before = fs::read(fixture_root.root.join("state/state.json")).unwrap();
        assert!(migrate_state(&fixture_root.repository, &fixture_root.context).is_err());
        assert_eq!(
            fs::read(fixture_root.root.join("state/state.json")).unwrap(),
            before
        );
        fixture_root.remove();
    }

    #[test]
    fn state_migration_rejects_include_cycles_without_publishing_the_barrier() {
        let fixture = migration_fixture("cycle", PROFILE, true);
        fs::write(
            fixture.root.join("profiles/base.yaml"),
            "schema_version: 3\nid: base\nincludes: [{ id: extra }]\nresources: {}\n",
        )
        .unwrap();
        fs::write(
            fixture.root.join("profiles/extra.yaml"),
            "schema_version: 3\nid: extra\nincludes: [{ id: base }]\nresources: {}\n",
        )
        .unwrap();
        let state_path = fixture.root.join("state/state.json");
        let before = fs::read(&state_path).unwrap();

        assert!(matches!(
            migrate_state(&fixture.repository, &fixture.context),
            Err(MigrationExecutionError::Migration(
                MigrationError::IncludeCycle { .. }
            ))
        ));
        assert_eq!(fs::read(&state_path).unwrap(), before);
        fixture.remove();
    }

    #[test]
    fn state_migration_facade_rejects_unsupported_legacy_resource_effects_without_writing_state() {
        for (label, profile) in [
            (
                "unsupported-kind",
                PROFILE.replace("kind: file", "kind: device"),
            ),
            (
                "unsupported-operation",
                PROFILE.replace("operation: link", "operation: synchronize"),
            ),
            (
                "directory-copy",
                PROFILE
                    .replace("kind: file", "kind: directory")
                    .replace("operation: link", "operation: copy"),
            ),
        ] {
            let fixture = migration_fixture(label, &profile, true);
            let state_path = fixture.root.join("state/state.json");
            let before = fs::read(&state_path).unwrap();

            assert!(migrate_state(&fixture.repository, &fixture.context).is_err());
            assert_eq!(
                fs::read(&state_path).unwrap(),
                before,
                "{label} state changed"
            );

            fixture.remove();
        }
    }

    #[test]
    fn state_migration_preserves_original_bytes_for_every_rejected_unknown_legacy_field() {
        for (label, unsupported) in [
            (
                "root",
                STATE.replace("\"resources\":", "\"unexpected\":true,\"resources\":"),
            ),
            (
                "resource",
                STATE.replace("\"effect\":{", "\"unexpected\":true,\"effect\":{"),
            ),
            (
                "effect",
                STATE.replace(
                    "\"kind\":\"file_link\",",
                    "\"kind\":\"file_link\",\"unexpected\":true,",
                ),
            ),
        ] {
            let fixture = migration_fixture(&format!("unknown-state-field-{label}"), PROFILE, true);
            let state_path = fixture.root.join("state/state.json");
            fs::write(&state_path, unsupported.as_bytes()).unwrap();

            assert!(migrate_state(&fixture.repository, &fixture.context).is_err());
            assert_eq!(
                fs::read(&state_path).unwrap(),
                unsupported.as_bytes(),
                "{label} rejection must not rewrite the original state bytes"
            );
            fixture.remove();
        }
    }

    #[test]
    fn facade_registers_and_executes_only_the_state_route() {
        let fixture = migration_fixture("facade-state-route", PROFILE, true);
        let outcome = super::super::MigrationFacade::execute(super::super::MigrationRequest::new(
            super::super::MigrationOperation::State,
            fixture.context.clone(),
        ))
        .unwrap();

        assert_eq!(outcome.operation, super::super::MigrationOperation::State);
        let migrated: JsonValue =
            serde_json::from_slice(&fs::read(fixture.root.join("state/state.json")).unwrap())
                .unwrap();
        assert_eq!(migrated["schema_version"], CURRENT_SCHEMA_VERSION);
        fixture.remove();
    }

    #[test]
    fn facade_registers_and_executes_the_profile_operation_on_the_same_route() {
        let fixture = migration_fixture("facade-profile-route", PROFILE, true);
        fs::remove_file(fixture.root.join("state/state.json")).unwrap();
        let outcome = super::super::MigrationFacade::execute(super::super::MigrationRequest::new(
            super::super::MigrationOperation::Profiles,
            fixture.context.clone(),
        ))
        .unwrap();

        assert_eq!(
            outcome.operation,
            super::super::MigrationOperation::Profiles
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("profiles/base.yaml")).unwrap(),
            PROFILE.replacen("schema_version: 3", "schema_version: 4", 1)
        );
        fixture.remove();
    }

    #[test]
    fn state_migration_rejects_a_changed_inventory_before_publication() {
        let fixture = migration_fixture("changed", PROFILE, true);
        let before = fs::read(fixture.root.join("state/state.json")).unwrap();
        let profile_path = fixture.root.join("profiles/base.yaml");
        let result =
            migrate_state_after_initial_inventory(&fixture.repository, &fixture.context, || {
                fs::write(&profile_path, PROFILE.replace("id: base", "id: changed")).unwrap();
            });
        assert!(matches!(
            result,
            Err(MigrationExecutionError::Migration(
                MigrationError::ProfileInventoryChanged
            ))
        ));
        assert_eq!(
            fs::read(fixture.root.join("state/state.json")).unwrap(),
            before
        );
        fixture.remove();
    }

    #[test]
    fn state_migration_lock_contention_leaves_the_legacy_state_byte_for_byte_unchanged() {
        let fixture = migration_fixture("state-lock-contention", PROFILE, true);
        let state_path = fixture.root.join("state/state.json");
        let before = fs::read(&state_path).unwrap();
        let _held_lock = fixture
            .repository
            .acquire_exclusive_for_migration()
            .unwrap();

        assert!(matches!(
            migrate_state(&fixture.repository, &fixture.context),
            Err(MigrationExecutionError::State(
                StateRepositoryError::LockContended { .. }
            ))
        ));
        assert_eq!(fs::read(&state_path).unwrap(), before);
        drop(_held_lock);
        fixture.remove();
    }

    #[test]
    fn profile_migration_requires_an_absent_or_current_state_barrier_and_changes_only_version() {
        let fixture = migration_fixture("profiles-absent-state", PROFILE, true);
        let state_path = fixture.root.join("state/state.json");
        fs::remove_file(&state_path).unwrap();
        let profile_path = fixture.root.join("profiles/base.yaml");
        let before = fs::read_to_string(&profile_path).unwrap();

        migrate_profiles(&fixture.repository, &fixture.context).unwrap();
        assert_eq!(
            fs::read_to_string(&profile_path).unwrap(),
            before.replacen("schema_version: 3", "schema_version: 4", 1)
        );
        assert!(!state_path.exists());
        fixture.remove();

        let fixture = migration_fixture("profiles-legacy-state", PROFILE, true);
        let profile_path = fixture.root.join("profiles/base.yaml");
        let before = fs::read_to_string(&profile_path).unwrap();
        assert!(migrate_profiles(&fixture.repository, &fixture.context).is_err());
        assert_eq!(fs::read_to_string(&profile_path).unwrap(), before);
        fixture.remove();
    }

    #[test]
    fn profile_migration_preflights_every_document_before_publication() {
        let fixture = migration_fixture("profile-preflight", PROFILE, true);
        fs::remove_file(fixture.root.join("state/state.json")).unwrap();
        let base_path = fixture.root.join("profiles/base.yaml");
        let before = fs::read(&base_path).unwrap();
        fs::write(
            fixture.root.join("profiles/invalid.yaml"),
            "schema_version: 3\nid: invalid\nresources: []\n",
        )
        .unwrap();

        assert!(migrate_profiles(&fixture.repository, &fixture.context).is_err());
        assert_eq!(fs::read(&base_path).unwrap(), before);
        fixture.remove();
    }

    #[test]
    fn profile_migration_rejects_include_cycles_before_any_publication() {
        let fixture = migration_fixture("profile-cycle", PROFILE, true);
        fs::remove_file(fixture.root.join("state/state.json")).unwrap();
        let base_path = fixture.root.join("profiles/base.yaml");
        let before = fs::read(&base_path).unwrap();
        fs::write(
            &base_path,
            "schema_version: 3\nid: base\nincludes: [{ id: extra }]\nresources: {}\n",
        )
        .unwrap();
        let expected = fs::read(&base_path).unwrap();
        fs::write(
            fixture.root.join("profiles/extra.yaml"),
            "schema_version: 3\nid: extra\nincludes: [{ id: base }]\nresources: {}\n",
        )
        .unwrap();

        assert!(matches!(
            migrate_profiles(&fixture.repository, &fixture.context),
            Err(MigrationExecutionError::Migration(
                MigrationError::IncludeCycle { .. }
            ))
        ));
        assert_eq!(fs::read(&base_path).unwrap(), expected);
        assert_ne!(fs::read(&base_path).unwrap(), before);
        fixture.remove();
    }

    struct MigrationFixture {
        root: PathBuf,
        context: ResolverContext,
        repository: StateRepository,
    }

    impl MigrationFixture {
        fn remove(self) {
            fs::remove_dir_all(self.root).unwrap();
        }
    }

    fn migration_fixture(label: &str, profile: &str, create_profiles: bool) -> MigrationFixture {
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "loadout-migration-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("state")).unwrap();
        if create_profiles {
            fs::create_dir_all(root.join("profiles")).unwrap();
            fs::write(root.join("profiles/base.yaml"), profile).unwrap();
        }
        fs::write(
            root.join("config.yaml"),
            "schema_version: 2\nprofile_discovery:\n  paths: [profiles]\nstores: {}\n",
        )
        .unwrap();
        fs::write(root.join("state/state.json"), STATE).unwrap();
        let context = ResolverContext::new(
            root.join("home"),
            root.join("runtime.yaml"),
            root.join("config.yaml"),
            root.join("state"),
        )
        .unwrap();
        let repository = StateRepository::new(ResolvedPath::new(root.join("state")).unwrap());
        MigrationFixture {
            root,
            context,
            repository,
        }
    }
}
