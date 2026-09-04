//! Profile discovery, composition, path binding, and static validation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::declaration::environment_config::EnvironmentConfig;
use crate::declaration::profile::ProfileDeclaration;
use crate::domain::desired::{ResolvedDesired, ResolvedDesiredError};
use crate::domain::file_link::{ResolvedFileLink, ResolvedFileLinkError};
use crate::domain::ids::{
    FullyQualifiedResourceId, IdentifierError, ProfileId, ResourceId, StoreId,
};
use crate::domain::paths::{
    ResolvedPath, ResolvedPathError, SourceRelativePath, has_windows_drive_prefix,
};
use crate::filesystem::is_link_or_reparse_point;
use crate::inspection::source::{
    PhysicalStoreRoot, SourceVerificationError, VerifiedSource, resolve_store_root,
    verify_regular_source,
};

/// Canonical Desired state and the source proofs needed to execute it safely.
///
/// The planner receives only `desired`. The accompanying source proofs remain outside the planner boundary, but retain the physical store-root and relative-path facts needed for the executor's immediate recheck.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedApplyInput {
    desired: ResolvedDesired,
    verified_sources: BTreeMap<FullyQualifiedResourceId, VerifiedSource>,
}

impl ResolvedApplyInput {
    /// Returns the canonical Desired input for inspection and pure planning.
    pub(crate) fn desired(&self) -> &ResolvedDesired {
        &self.desired
    }

    /// Returns the source proofs keyed by the resource identities they support.
    pub(crate) fn verified_sources(&self) -> &BTreeMap<FullyQualifiedResourceId, VerifiedSource> {
        &self.verified_sources
    }

    fn into_desired(self) -> ResolvedDesired {
        self.desired
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        desired: ResolvedDesired,
        verified_sources: BTreeMap<FullyQualifiedResourceId, VerifiedSource>,
    ) -> Self {
        Self {
            desired,
            verified_sources,
        }
    }
}

/// Immutable machine paths required to bind one environment configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolverContext {
    home_directory: ResolvedPath,
    runtime_config_path: ResolvedPath,
    environment_config_path: ResolvedPath,
    state_directory: ResolvedPath,
}

impl ResolverContext {
    /// Creates a context from already selected, absolute control paths.
    pub(crate) fn new(
        home_directory: PathBuf,
        runtime_config_path: PathBuf,
        environment_config_path: PathBuf,
        state_directory: PathBuf,
    ) -> Result<Self, ResolvedPathError> {
        Ok(Self {
            home_directory: ResolvedPath::new(home_directory)?,
            runtime_config_path: ResolvedPath::new(runtime_config_path)?,
            environment_config_path: ResolvedPath::new(environment_config_path)?,
            state_directory: ResolvedPath::new(state_directory)?,
        })
    }
}

/// Resolves one selected root profile to canonical Desired resources.
///
/// The resolver reads only environment/profile declarations and verified local store sources. It never observes a managed target or performs mutation.
pub(crate) fn resolve(
    context: &ResolverContext,
    environment: &EnvironmentConfig,
    selected_root_profile: Option<&str>,
) -> Result<ResolvedDesired, ResolverError> {
    resolve_for_apply(context, environment, selected_root_profile)
        .map(ResolvedApplyInput::into_desired)
}

/// Resolves one selected root profile and retains the verified source facts required by a later non-dry-run apply.
///
/// This has the same declaration and source-validation behavior as [`resolve`] and still does not inspect managed targets or mutate state.
pub(crate) fn resolve_for_apply(
    context: &ResolverContext,
    environment: &EnvironmentConfig,
    selected_root_profile: Option<&str>,
) -> Result<ResolvedApplyInput, ResolverError> {
    let profiles = discover_profiles(context, environment)?;
    let root_profile = select_root_profile(environment, selected_root_profile, &profiles)?;
    let composed_profiles = compose_profiles(&root_profile, &profiles)?;
    let stores = resolve_stores(context, environment)?;
    let profile_paths = profiles
        .values()
        .map(|profile| profile.file_path.clone())
        .collect::<Vec<_>>();
    let protected_paths = ProtectedPaths::new(context, profile_paths)?;

    let mut resources = Vec::new();
    let mut verified_sources = BTreeMap::new();
    for profile_id in composed_profiles {
        let profile = profiles
            .get(&profile_id)
            .expect("composed profiles must have been discovered");
        for (raw_resource_id, resource) in profile.declaration.resources() {
            let resource_id = ResourceId::parse(raw_resource_id.to_owned()).map_err(|source| {
                ResolverError::InvalidIdentifier {
                    role: "resource ID",
                    value: raw_resource_id.to_owned(),
                    source,
                }
            })?;
            let resource_id = FullyQualifiedResourceId::new(&profile_id, &resource_id);
            let properties = resource.properties();
            let raw_store_id = properties.source().store();
            let store_id = StoreId::parse(raw_store_id.to_owned()).map_err(|source| {
                ResolverError::InvalidIdentifier {
                    role: "store ID",
                    value: raw_store_id.to_owned(),
                    source,
                }
            })?;
            let store = stores
                .get(&store_id)
                .ok_or_else(|| ResolverError::MissingStore {
                    resource_id: resource_id.clone(),
                    store_id,
                })?;
            let source_components = parse_source_path(properties.source().path())?;
            let verified_source =
                verify_regular_source(&store.root, &source_components).map_err(|source| {
                    ResolverError::SourceVerification {
                        resource_id: resource_id.clone(),
                        source,
                    }
                })?;
            let target_path = bind_target_path(properties.target(), context)?;
            protected_paths.ensure_target_allowed(&target_path, &stores)?;

            let resolved = ResolvedFileLink::new(
                resource_id.clone(),
                verified_source.path().clone(),
                target_path,
            )
            .map_err(ResolverError::InvalidResolvedFileLink)?;
            resources.push(resolved);
            verified_sources.insert(resource_id, verified_source);
        }
    }

    let desired =
        ResolvedDesired::new(root_profile, resources).map_err(ResolverError::InvalidDesired)?;
    Ok(ResolvedApplyInput {
        desired,
        verified_sources,
    })
}

#[derive(Debug)]
struct DiscoveredProfile {
    declaration: ProfileDeclaration,
    file_path: ResolvedPath,
}

fn discover_profiles(
    context: &ResolverContext,
    environment: &EnvironmentConfig,
) -> Result<BTreeMap<ProfileId, DiscoveredProfile>, ResolverError> {
    let config_directory = context
        .environment_config_path
        .as_ref()
        .parent()
        .ok_or(ResolverError::EnvironmentConfigHasNoParent)?;
    let mut candidates = BTreeMap::<ProfileId, Vec<DiscoveredProfile>>::new();

    for raw_path in environment.profile_discovery_paths() {
        let discovery_path = bind_configuration_path(raw_path, config_directory, context)?;
        let declared_metadata = fs::symlink_metadata(&discovery_path).map_err(|source| {
            ResolverError::ProfileDiscoveryIo {
                path: discovery_path.clone(),
                source,
            }
        })?;
        if is_link_or_reparse_point(&declared_metadata) {
            return Err(ResolverError::ProfileDiscoveryLinkOrReparsePoint {
                path: discovery_path,
            });
        }
        if !declared_metadata.is_dir() {
            return Err(ResolverError::ProfileDiscoveryNotDirectory {
                path: discovery_path,
            });
        }
        let physical_path = fs::canonicalize(&discovery_path).map_err(|source| {
            ResolverError::ProfileDiscoveryIo {
                path: discovery_path.clone(),
                source,
            }
        })?;

        let entries =
            fs::read_dir(&physical_path).map_err(|source| ResolverError::ProfileDiscoveryIo {
                path: physical_path.clone(),
                source,
            })?;
        for entry in entries {
            let entry = entry.map_err(|source| ResolverError::ProfileDiscoveryIo {
                path: physical_path.clone(),
                source,
            })?;
            let profile_path = entry.path();
            let metadata = fs::symlink_metadata(&profile_path).map_err(|source| {
                ResolverError::ProfileDiscoveryIo {
                    path: profile_path.clone(),
                    source,
                }
            })?;
            if is_link_or_reparse_point(&metadata) || !metadata.is_file() {
                continue;
            }
            if profile_path
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("yaml")
            {
                continue;
            }
            let yaml =
                fs::read_to_string(&profile_path).map_err(|source| ResolverError::ReadProfile {
                    path: profile_path.clone(),
                    source,
                })?;
            let declaration = ProfileDeclaration::parse(&yaml).map_err(|source| {
                ResolverError::InvalidProfileSchema {
                    path: profile_path.clone(),
                    source,
                }
            })?;
            let id = ProfileId::parse(declaration.id().to_owned()).map_err(|source| {
                ResolverError::InvalidIdentifier {
                    role: "profile ID",
                    value: declaration.id().to_owned(),
                    source,
                }
            })?;
            let file_path = ResolvedPath::new(profile_path).map_err(ResolverError::InvalidPath)?;
            candidates.entry(id).or_default().push(DiscoveredProfile {
                declaration,
                file_path,
            });
        }
    }

    if let Some((profile_id, profiles)) = candidates.iter().find(|(_, profiles)| profiles.len() > 1)
    {
        return Err(ResolverError::DuplicateProfileId {
            profile_id: profile_id.clone(),
            files: profiles
                .iter()
                .map(|profile| profile.file_path.clone())
                .collect(),
        });
    }

    Ok(candidates
        .into_iter()
        .map(|(profile_id, mut profiles)| {
            (
                profile_id,
                profiles
                    .pop()
                    .expect("each discovered profile ID must have one declaration"),
            )
        })
        .collect())
}

fn select_root_profile(
    environment: &EnvironmentConfig,
    selected_root_profile: Option<&str>,
    profiles: &BTreeMap<ProfileId, DiscoveredProfile>,
) -> Result<ProfileId, ResolverError> {
    let raw_root_profile = match selected_root_profile {
        Some(profile_id) => profile_id,
        None => environment
            .default_profile()
            .ok_or(ResolverError::MissingRootProfile)?,
    };
    let root_profile = ProfileId::parse(raw_root_profile.to_owned()).map_err(|source| {
        ResolverError::InvalidIdentifier {
            role: "root profile ID",
            value: raw_root_profile.to_owned(),
            source,
        }
    })?;
    if !profiles.contains_key(&root_profile) {
        return Err(ResolverError::RootProfileNotDiscovered { root_profile });
    }

    Ok(root_profile)
}

fn compose_profiles(
    root_profile: &ProfileId,
    profiles: &BTreeMap<ProfileId, DiscoveredProfile>,
) -> Result<Vec<ProfileId>, ResolverError> {
    fn visit(
        profile_id: &ProfileId,
        profiles: &BTreeMap<ProfileId, DiscoveredProfile>,
        visiting: &mut Vec<ProfileId>,
        visited: &mut BTreeSet<ProfileId>,
        output: &mut Vec<ProfileId>,
    ) -> Result<(), ResolverError> {
        if visited.contains(profile_id) {
            return Ok(());
        }
        if let Some(cycle_start) = visiting.iter().position(|current| current == profile_id) {
            let mut cycle = visiting[cycle_start..].to_vec();
            cycle.push(profile_id.clone());
            return Err(ResolverError::IncludeCycle { cycle });
        }

        let profile = profiles
            .get(profile_id)
            .expect("included profiles must be checked before traversal");
        visiting.push(profile_id.clone());
        for include in profile.declaration.includes() {
            let included_profile = ProfileId::parse(include.id().to_owned()).map_err(|source| {
                ResolverError::InvalidIdentifier {
                    role: "included profile ID",
                    value: include.id().to_owned(),
                    source,
                }
            })?;
            if !profiles.contains_key(&included_profile) {
                return Err(ResolverError::MissingIncludedProfile {
                    including_profile: profile_id.clone(),
                    included_profile,
                });
            }
            visit(&included_profile, profiles, visiting, visited, output)?;
        }
        visiting.pop();
        visited.insert(profile_id.clone());
        output.push(profile_id.clone());
        Ok(())
    }

    let mut visiting = Vec::new();
    let mut visited = BTreeSet::new();
    let mut output = Vec::new();
    visit(
        root_profile,
        profiles,
        &mut visiting,
        &mut visited,
        &mut output,
    )?;
    Ok(output)
}

#[derive(Debug)]
struct ResolvedStore {
    root: PhysicalStoreRoot,
}

fn resolve_stores(
    context: &ResolverContext,
    environment: &EnvironmentConfig,
) -> Result<BTreeMap<StoreId, ResolvedStore>, ResolverError> {
    let config_directory = context
        .environment_config_path
        .as_ref()
        .parent()
        .ok_or(ResolverError::EnvironmentConfigHasNoParent)?;
    let mut stores = BTreeMap::new();

    for (raw_store_id, store) in environment.stores() {
        let store_id = StoreId::parse(raw_store_id.to_owned()).map_err(|source| {
            ResolverError::InvalidIdentifier {
                role: "store ID",
                value: raw_store_id.to_owned(),
                source,
            }
        })?;
        let declared_root = bind_configuration_path(store.path(), config_directory, context)?;
        let root = resolve_store_root(&declared_root).map_err(|source| {
            ResolverError::StoreVerification {
                store_id: store_id.clone(),
                source,
            }
        })?;
        stores.insert(store_id, ResolvedStore { root });
    }

    Ok(stores)
}

fn bind_configuration_path(
    raw_path: &str,
    relative_base: &Path,
    context: &ResolverContext,
) -> Result<PathBuf, ResolverError> {
    if raw_path == "~"
        || (raw_path.starts_with('~') && !raw_path.starts_with("~/"))
        || has_windows_drive_prefix(raw_path)
    {
        return Err(ResolverError::InvalidConfigurationPath {
            value: raw_path.to_owned(),
        });
    }
    if let Some(relative_to_home) = raw_path.strip_prefix("~/") {
        return Ok(context.home_directory.as_ref().join(relative_to_home));
    }

    let path = Path::new(raw_path);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(relative_base.join(path))
    }
}

fn parse_source_path(raw_path: &str) -> Result<SourceRelativePath, ResolverError> {
    SourceRelativePath::parse(raw_path).map_err(|_| ResolverError::InvalidSourcePath {
        value: raw_path.to_owned(),
    })
}

fn bind_target_path(
    raw_target: &str,
    context: &ResolverContext,
) -> Result<ResolvedPath, ResolverError> {
    let raw_path = if let Some(relative_to_home) = raw_target.strip_prefix("~/") {
        context.home_directory.as_ref().join(relative_to_home)
    } else {
        let path = Path::new(raw_target);
        if !path.is_absolute() {
            return Err(ResolverError::InvalidTargetPath {
                value: raw_target.to_owned(),
            });
        }
        path.to_path_buf()
    };
    if raw_target
        .strip_prefix("~/")
        .unwrap_or(raw_target)
        .split('/')
        .any(|component| component == "." || component == "..")
        || raw_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(ResolverError::InvalidTargetPath {
            value: raw_target.to_owned(),
        });
    }

    let target_path = ResolvedPath::new(raw_path).map_err(ResolverError::InvalidPath)?;
    if !target_path
        .as_ref()
        .starts_with(context.home_directory.as_ref())
    {
        return Err(ResolverError::TargetOutsideHome { target_path });
    }
    Ok(target_path)
}

struct ProtectedPaths {
    declared_home: ResolvedPath,
    canonical_home: ResolvedPath,
    runtime_config_path: ResolvedPath,
    environment_config_path: ResolvedPath,
    runtime_config_directory: ResolvedPath,
    state_directory: ResolvedPath,
    profile_paths: Vec<ResolvedPath>,
}

impl ProtectedPaths {
    fn new(
        context: &ResolverContext,
        profile_paths: Vec<ResolvedPath>,
    ) -> Result<Self, ResolverError> {
        let canonical_home = canonical_home_directory(&context.home_directory)?;
        let runtime_config_path = resolve_home_alias(
            &context.home_directory,
            &canonical_home,
            &context.runtime_config_path,
        )?;
        let environment_config_path = resolve_home_alias(
            &context.home_directory,
            &canonical_home,
            &context.environment_config_path,
        )?;
        let state_directory = resolve_home_alias(
            &context.home_directory,
            &canonical_home,
            &context.state_directory,
        )?;
        let profile_paths = profile_paths
            .into_iter()
            .map(|path| resolve_home_alias(&context.home_directory, &canonical_home, &path))
            .collect::<Result<Vec<_>, _>>()?;
        let runtime_config_directory = runtime_config_path
            .as_ref()
            .parent()
            .ok_or(ResolverError::RuntimeConfigHasNoParent)?;
        let runtime_config_directory = ResolvedPath::new(runtime_config_directory.to_path_buf())
            .map_err(ResolverError::InvalidPath)?;
        Ok(Self {
            declared_home: context.home_directory.clone(),
            canonical_home,
            runtime_config_path,
            environment_config_path,
            runtime_config_directory,
            state_directory,
            profile_paths,
        })
    }

    fn ensure_target_allowed(
        &self,
        target_path: &ResolvedPath,
        stores: &BTreeMap<StoreId, ResolvedStore>,
    ) -> Result<(), ResolverError> {
        let physical_target_path =
            resolve_home_alias(&self.declared_home, &self.canonical_home, target_path)?;
        let protected = [
            (&self.runtime_config_path, "runtime configuration file"),
            (
                &self.environment_config_path,
                "environment configuration file",
            ),
        ];
        if let Some((_, protected_by)) = protected
            .iter()
            .find(|(path, _)| *path == &physical_target_path)
        {
            return Err(ResolverError::ProtectedTarget {
                target_path: target_path.clone(),
                protected_by,
            });
        }
        if physical_target_path
            .as_ref()
            .starts_with(self.runtime_config_directory.as_ref())
        {
            return Err(ResolverError::ProtectedTarget {
                target_path: target_path.clone(),
                protected_by: "runtime configuration directory",
            });
        }
        if physical_target_path
            .as_ref()
            .starts_with(self.state_directory.as_ref())
        {
            return Err(ResolverError::ProtectedTarget {
                target_path: target_path.clone(),
                protected_by: "state directory",
            });
        }
        if self
            .profile_paths
            .iter()
            .any(|path| path == &physical_target_path)
        {
            return Err(ResolverError::ProtectedTarget {
                target_path: target_path.clone(),
                protected_by: "profile file",
            });
        }
        if stores.values().any(|store| {
            physical_target_path
                .as_ref()
                .starts_with(store.root.as_path().as_ref())
        }) {
            return Err(ResolverError::ProtectedTarget {
                target_path: target_path.clone(),
                protected_by: "local store root",
            });
        }

        Ok(())
    }
}

fn canonical_home_directory(home_directory: &ResolvedPath) -> Result<ResolvedPath, ResolverError> {
    let canonical_home = fs::canonicalize(home_directory.as_ref()).map_err(|source| {
        ResolverError::HomeDirectoryIo {
            path: home_directory.as_ref().to_path_buf(),
            source,
        }
    })?;
    let metadata =
        fs::metadata(&canonical_home).map_err(|source| ResolverError::HomeDirectoryIo {
            path: canonical_home.clone(),
            source,
        })?;
    if !metadata.is_dir() {
        return Err(ResolverError::HomeDirectoryNotDirectory {
            path: canonical_home,
        });
    }

    ResolvedPath::new(canonical_home).map_err(ResolverError::InvalidPath)
}

fn resolve_home_alias(
    declared_home: &ResolvedPath,
    canonical_home: &ResolvedPath,
    path: &ResolvedPath,
) -> Result<ResolvedPath, ResolverError> {
    if path.as_ref().starts_with(canonical_home.as_ref()) {
        return Ok(path.clone());
    }
    let Ok(relative_path) = path.as_ref().strip_prefix(declared_home.as_ref()) else {
        return Ok(path.clone());
    };

    ResolvedPath::new(canonical_home.as_ref().join(relative_path))
        .map_err(ResolverError::InvalidPath)
}

/// The reason declarations cannot be resolved to safe canonical Desired input.
#[derive(Debug)]
pub(crate) enum ResolverError {
    EnvironmentConfigHasNoParent,
    RuntimeConfigHasNoParent,
    HomeDirectoryIo {
        path: PathBuf,
        source: io::Error,
    },
    HomeDirectoryNotDirectory {
        path: PathBuf,
    },
    InvalidPath(ResolvedPathError),
    InvalidConfigurationPath {
        value: String,
    },
    ProfileDiscoveryIo {
        path: PathBuf,
        source: io::Error,
    },
    ProfileDiscoveryNotDirectory {
        path: PathBuf,
    },
    ProfileDiscoveryLinkOrReparsePoint {
        path: PathBuf,
    },
    ReadProfile {
        path: PathBuf,
        source: io::Error,
    },
    InvalidProfileSchema {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    InvalidIdentifier {
        role: &'static str,
        value: String,
        source: IdentifierError,
    },
    DuplicateProfileId {
        profile_id: ProfileId,
        files: Vec<ResolvedPath>,
    },
    MissingRootProfile,
    RootProfileNotDiscovered {
        root_profile: ProfileId,
    },
    MissingIncludedProfile {
        including_profile: ProfileId,
        included_profile: ProfileId,
    },
    IncludeCycle {
        cycle: Vec<ProfileId>,
    },
    StoreVerification {
        store_id: StoreId,
        source: SourceVerificationError,
    },
    MissingStore {
        resource_id: FullyQualifiedResourceId,
        store_id: StoreId,
    },
    InvalidSourcePath {
        value: String,
    },
    SourceVerification {
        resource_id: FullyQualifiedResourceId,
        source: SourceVerificationError,
    },
    InvalidTargetPath {
        value: String,
    },
    TargetOutsideHome {
        target_path: ResolvedPath,
    },
    ProtectedTarget {
        target_path: ResolvedPath,
        protected_by: &'static str,
    },
    InvalidResolvedFileLink(ResolvedFileLinkError),
    InvalidDesired(ResolvedDesiredError),
}

impl fmt::Display for ResolverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvironmentConfigHasNoParent => {
                formatter.write_str("environment configuration path has no parent directory")
            }
            Self::RuntimeConfigHasNoParent => {
                formatter.write_str("runtime configuration path has no parent directory")
            }
            Self::HomeDirectoryIo { path, source } => {
                write!(
                    formatter,
                    "cannot access home directory {}: {source}",
                    path.display()
                )
            }
            Self::HomeDirectoryNotDirectory { path } => {
                write!(
                    formatter,
                    "home directory is not a directory: {}",
                    path.display()
                )
            }
            Self::InvalidPath(error) => error.fmt(formatter),
            Self::InvalidConfigurationPath { value } => {
                write!(formatter, "invalid configuration-level path: {value}")
            }
            Self::ProfileDiscoveryIo { path, source } => {
                write!(
                    formatter,
                    "cannot inspect profile discovery path {}: {source}",
                    path.display()
                )
            }
            Self::ProfileDiscoveryNotDirectory { path } => {
                write!(
                    formatter,
                    "profile discovery path is not a directory: {}",
                    path.display()
                )
            }
            Self::ProfileDiscoveryLinkOrReparsePoint { path } => {
                write!(
                    formatter,
                    "profile discovery path must not be a link or reparse point: {}",
                    path.display()
                )
            }
            Self::ReadProfile { path, source } => {
                write!(
                    formatter,
                    "cannot read profile file {}: {source}",
                    path.display()
                )
            }
            Self::InvalidProfileSchema { path, source } => {
                write!(
                    formatter,
                    "invalid profile schema in {}: {source}",
                    path.display()
                )
            }
            Self::InvalidIdentifier {
                role,
                value,
                source,
            } => write!(formatter, "invalid {role} {value}: {source}"),
            Self::DuplicateProfileId { profile_id, files } => {
                write!(
                    formatter,
                    "duplicate discovered profile ID {profile_id} in {files:?}"
                )
            }
            Self::MissingRootProfile => formatter.write_str("no root profile was selected"),
            Self::RootProfileNotDiscovered { root_profile } => {
                write!(
                    formatter,
                    "selected root profile was not discovered: {root_profile}"
                )
            }
            Self::MissingIncludedProfile {
                including_profile,
                included_profile,
            } => write!(
                formatter,
                "profile {including_profile} includes undiscovered profile {included_profile}"
            ),
            Self::IncludeCycle { cycle } => write!(formatter, "profile include cycle: {cycle:?}"),
            Self::StoreVerification { store_id, source } => {
                write!(formatter, "invalid local store {store_id}: {source}")
            }
            Self::MissingStore {
                resource_id,
                store_id,
            } => write!(
                formatter,
                "resource {resource_id} references missing store {store_id}"
            ),
            Self::InvalidSourcePath { value } => write!(formatter, "invalid source path: {value}"),
            Self::SourceVerification {
                resource_id,
                source,
            } => write!(
                formatter,
                "resource {resource_id} has invalid source: {source}"
            ),
            Self::InvalidTargetPath { value } => write!(formatter, "invalid target path: {value}"),
            Self::TargetOutsideHome { target_path } => {
                write!(
                    formatter,
                    "target is outside the current home directory: {target_path}"
                )
            }
            Self::ProtectedTarget {
                target_path,
                protected_by,
            } => write!(
                formatter,
                "target {target_path} is protected {protected_by}"
            ),
            Self::InvalidResolvedFileLink(error) => error.fmt(formatter),
            Self::InvalidDesired(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ResolverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidPath(error) => Some(error),
            Self::HomeDirectoryIo { source, .. } => Some(source),
            Self::ProfileDiscoveryIo { source, .. } | Self::ReadProfile { source, .. } => {
                Some(source)
            }
            Self::InvalidProfileSchema { source, .. } => Some(source),
            Self::InvalidIdentifier { source, .. } => Some(source),
            Self::StoreVerification { source, .. } | Self::SourceVerification { source, .. } => {
                Some(source)
            }
            Self::InvalidResolvedFileLink(error) => Some(error),
            Self::InvalidDesired(error) => Some(error),
            Self::EnvironmentConfigHasNoParent
            | Self::RuntimeConfigHasNoParent
            | Self::HomeDirectoryNotDirectory { .. }
            | Self::InvalidConfigurationPath { .. }
            | Self::ProfileDiscoveryNotDirectory { .. }
            | Self::ProfileDiscoveryLinkOrReparsePoint { .. }
            | Self::DuplicateProfileId { .. }
            | Self::MissingRootProfile
            | Self::RootProfileNotDiscovered { .. }
            | Self::MissingIncludedProfile { .. }
            | Self::IncludeCycle { .. }
            | Self::MissingStore { .. }
            | Self::InvalidSourcePath { .. }
            | Self::InvalidTargetPath { .. }
            | Self::TargetOutsideHome { .. }
            | Self::ProtectedTarget { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::declaration::environment_config::EnvironmentConfig;

    static NEXT_WORKSPACE_ID: AtomicU64 = AtomicU64::new(0);

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let unique_id = NEXT_WORKSPACE_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "loadout-resolver-test-{}-{timestamp}-{unique_id}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();

            Self { root }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }

        fn create_dir(&self, relative: &str) {
            fs::create_dir_all(self.path(relative)).unwrap();
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn context(&self) -> ResolverContext {
            self.create_dir("home");
            self.create_dir("config");
            self.create_dir("state");
            self.write("config/loadout.yaml", "schema_version: 1\n");
            self.write("config/config.yaml", "schema_version: 1\n");

            ResolverContext::new(
                self.path("home"),
                self.path("config/loadout.yaml"),
                self.path("config/config.yaml"),
                self.path("state"),
            )
            .unwrap()
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn environment(
        default_profile: Option<&str>,
        discovery_paths: &[&str],
        store_path: &str,
    ) -> EnvironmentConfig {
        let default_profile = default_profile
            .map(|profile| format!("default_profile: {profile}\n"))
            .unwrap_or_default();
        let discovery_paths = discovery_paths
            .iter()
            .map(|path| format!("    - {path}\n"))
            .collect::<String>();
        EnvironmentConfig::parse(&format!(
            "schema_version: 1\n{default_profile}profile_discovery:\n  paths:\n{discovery_paths}stores:\n  dotfiles:\n    type: local\n    path: {store_path}\n"
        ))
        .unwrap()
    }

    fn environment_without_stores(
        default_profile: &str,
        discovery_paths: &[&str],
    ) -> EnvironmentConfig {
        let discovery_paths = discovery_paths
            .iter()
            .map(|path| format!("    - {path}\n"))
            .collect::<String>();
        EnvironmentConfig::parse(&format!(
            "schema_version: 1\ndefault_profile: {default_profile}\nprofile_discovery:\n  paths:\n{discovery_paths}stores: {{}}\n"
        ))
        .unwrap()
    }

    fn profile(id: &str, includes: &[&str], resources: &str) -> String {
        let includes = if includes.is_empty() {
            String::new()
        } else {
            let entries = includes
                .iter()
                .map(|include| format!("  - id: {include}\n"))
                .collect::<String>();
            format!("includes:\n{entries}")
        };
        format!("schema_version: 1\nid: {id}\n{includes}resources:\n{resources}")
    }

    fn file_resource(resource_id: &str, source: &str, target: &str) -> String {
        format!(
            "  {resource_id}:\n    type: file\n    properties:\n      kind: file\n      source:\n        store: dotfiles\n        path: {source}\n      target: {target}\n      operation: link\n"
        )
    }

    #[test]
    fn resolver_discovers_composes_and_binds_verified_resources_without_target_inspection() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        workspace.write("store/zsh/zshrc", "setopt autocd\n");
        workspace.write(
            "profiles/base.yaml",
            &profile(
                "base",
                &[],
                &file_resource("git", "git/config", "~/.gitconfig"),
            ),
        );
        workspace.write(
            "profiles/common.yaml",
            &profile(
                "common",
                &[],
                &file_resource("zsh", "zsh/zshrc", "~/.zshrc"),
            ),
        );
        workspace.write(
            "profiles/workstation.yaml",
            &profile(
                "workstation",
                &["base", "common", "base"],
                &file_resource("shell", "zsh/zshrc", "~/.uncreated/shellrc"),
            ),
        );
        let context = workspace.context();
        let environment = environment(Some("workstation"), &["../profiles"], "../store");
        let uncreated_target_parent = workspace.path("home/.uncreated");

        let resolved = resolve_for_apply(&context, &environment, None).unwrap();
        let desired = resolved.desired();

        assert_eq!(desired.root_profile().as_str(), "workstation");
        assert_eq!(
            desired
                .resources()
                .iter()
                .map(|resource| resource.resource_id().as_str())
                .collect::<Vec<_>>(),
            ["base/git", "common/zsh", "workstation/shell"]
        );
        assert_eq!(
            desired.resources()[0].source_path().as_ref(),
            fs::canonicalize(workspace.path("store/git/config")).unwrap()
        );
        assert_eq!(
            resolved
                .verified_sources()
                .get(&FullyQualifiedResourceId::parse("base/git").unwrap())
                .unwrap()
                .path()
                .as_ref(),
            fs::canonicalize(workspace.path("store/git/config")).unwrap()
        );
        assert_eq!(
            desired.resources()[2].target_path().as_ref(),
            workspace.path("home/.uncreated/shellrc")
        );
        assert!(
            !uncreated_target_parent.exists(),
            "resolver must not inspect or create target parents"
        );
    }

    #[test]
    fn resolver_rejects_duplicate_profile_ids_with_every_defining_file() {
        let workspace = TestWorkspace::new();
        workspace.write("profiles-a/one.yaml", &profile("base", &[], ""));
        workspace.write("profiles-b/two.yaml", &profile("base", &[], ""));
        let context = workspace.context();
        let environment = environment_without_stores("base", &["../profiles-a", "../profiles-b"]);

        let error = resolve(&context, &environment, None).unwrap_err();

        match error {
            ResolverError::DuplicateProfileId { profile_id, files } => {
                assert_eq!(profile_id.as_str(), "base");
                assert_eq!(files.len(), 2);
            }
            unexpected => panic!("unexpected resolver error: {unexpected:?}"),
        }
    }

    #[test]
    fn resolver_rejects_missing_includes_and_include_cycles() {
        let workspace = TestWorkspace::new();
        workspace.write("missing/base.yaml", &profile("base", &["absent"], ""));
        let context = workspace.context();
        let missing_environment = environment_without_stores("base", &["../missing"]);
        let missing_error = resolve(&context, &missing_environment, None).unwrap_err();
        assert!(matches!(
            missing_error,
            ResolverError::MissingIncludedProfile { .. }
        ));

        workspace.write("cycle/base.yaml", &profile("base", &["workstation"], ""));
        workspace.write(
            "cycle/workstation.yaml",
            &profile("workstation", &["base"], ""),
        );
        let cycle_environment = environment_without_stores("workstation", &["../cycle"]);
        let cycle_error = resolve(&context, &cycle_environment, None).unwrap_err();
        assert!(matches!(cycle_error, ResolverError::IncludeCycle { .. }));
    }

    #[test]
    fn resolver_rejects_duplicate_normalized_targets_after_profile_composition() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\n");
        workspace.write(
            "profiles/base.yaml",
            &profile(
                "base",
                &[],
                &file_resource("git", "git/config", "~/.gitconfig"),
            ),
        );
        workspace.write(
            "profiles/workstation.yaml",
            &profile(
                "workstation",
                &["base"],
                &file_resource("git-work", "git/config", "~/.gitconfig"),
            ),
        );
        let context = workspace.context();
        let environment = environment(Some("workstation"), &["../profiles"], "../store");

        let error = resolve(&context, &environment, None).unwrap_err();

        assert!(matches!(
            error,
            ResolverError::InvalidDesired(ResolvedDesiredError::DuplicateTarget { .. })
        ));
    }

    #[test]
    fn resolver_rejects_invalid_source_grammar_and_non_regular_sources_before_target_inspection() {
        let workspace = TestWorkspace::new();
        workspace.create_dir("store/a-directory");
        workspace.write(
            "profiles/workstation.yaml",
            &profile(
                "workstation",
                &[],
                &file_resource("bad-path", "git/../config", "~/.uncreated/target"),
            ),
        );
        let context = workspace.context();
        let environment = environment(Some("workstation"), &["../profiles"], "../store");

        let grammar_error = resolve(&context, &environment, None).unwrap_err();
        assert!(matches!(
            grammar_error,
            ResolverError::InvalidSourcePath { .. }
        ));

        workspace.write(
            "profiles/workstation.yaml",
            &profile(
                "workstation",
                &[],
                &file_resource("directory", "a-directory", "~/.uncreated/target"),
            ),
        );
        let non_regular_error = resolve(&context, &environment, None).unwrap_err();
        assert!(matches!(
            non_regular_error,
            ResolverError::SourceVerification {
                source: SourceVerificationError::SourceNotRegular { .. },
                ..
            }
        ));
        assert!(!workspace.path("home/.uncreated").exists());
    }

    #[test]
    fn resolver_source_path_grammar_rejects_each_forbidden_syntax_class() {
        for source_path in [
            "",
            "/absolute",
            "~/home-relative",
            "a//b",
            "a/./b",
            "a/../b",
            "a\\b",
            "C:/windows-prefix",
            "C:config",
        ] {
            assert!(
                matches!(
                    parse_source_path(source_path),
                    Err(ResolverError::InvalidSourcePath { .. })
                ),
                "{source_path:?} must not be a valid source path"
            );
        }
        assert!(parse_source_path("git/config").is_ok());
    }

    #[test]
    fn resolver_rejects_windows_drive_relative_configuration_paths() {
        let workspace = TestWorkspace::new();
        let context = workspace.context();

        let discovery_environment = environment_without_stores("base", &["C:profiles"]);
        let discovery_error = resolve(&context, &discovery_environment, None).unwrap_err();
        assert!(matches!(
            discovery_error,
            ResolverError::InvalidConfigurationPath { value } if value == "C:profiles"
        ));

        workspace.write("profiles/base.yaml", &profile("base", &[], ""));
        let store_environment = environment(Some("base"), &["../profiles"], "C:store");
        let store_error = resolve(&context, &store_environment, None).unwrap_err();
        assert!(matches!(
            store_error,
            ResolverError::InvalidConfigurationPath { value } if value == "C:store"
        ));
    }

    #[test]
    fn resolver_rejects_target_dot_components_before_any_target_observation() {
        let workspace = TestWorkspace::new();
        let context = workspace.context();

        for target_path in ["~/.config/./loadout", "~/.config/../loadout"] {
            assert!(
                matches!(
                    bind_target_path(target_path, &context),
                    Err(ResolverError::InvalidTargetPath { .. })
                ),
                "{target_path:?} must not be a valid target path"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolver_rejects_a_symlinked_source_parent() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("outside/config", "[user]\n");
        workspace.create_dir("store");
        symlink(workspace.path("outside"), workspace.path("store/linked")).unwrap();
        workspace.write(
            "profiles/workstation.yaml",
            &profile(
                "workstation",
                &[],
                &file_resource("git", "linked/config", "~/.gitconfig"),
            ),
        );
        let context = workspace.context();
        let environment = environment(Some("workstation"), &["../profiles"], "../store");

        let error = resolve(&context, &environment, None).unwrap_err();

        assert!(matches!(
            error,
            ResolverError::SourceVerification {
                source: SourceVerificationError::SourceParentLinkOrReparsePoint { .. },
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn resolver_rejects_a_symlinked_source_file() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("outside/config", "[user]\n");
        workspace.create_dir("store");
        symlink(
            workspace.path("outside/config"),
            workspace.path("store/config"),
        )
        .unwrap();
        workspace.write(
            "profiles/workstation.yaml",
            &profile(
                "workstation",
                &[],
                &file_resource("git", "config", "~/.gitconfig"),
            ),
        );
        let context = workspace.context();
        let environment = environment(Some("workstation"), &["../profiles"], "../store");

        let error = resolve(&context, &environment, None).unwrap_err();

        assert!(matches!(
            error,
            ResolverError::SourceVerification {
                source: SourceVerificationError::SourceLinkOrReparsePoint { .. },
                ..
            }
        ));
    }

    #[test]
    fn resolver_rejects_targets_that_name_control_or_store_paths() {
        let workspace = TestWorkspace::new();
        let _default_context = workspace.context();
        workspace.write("store/git/config", "[user]\n");
        workspace.write(
            "profiles/workstation.yaml",
            &profile(
                "workstation",
                &[],
                &file_resource("git", "git/config", "~/.config/loadout/config.yaml"),
            ),
        );
        workspace.write("home/.config/loadout/loadout.yaml", "schema_version: 1\n");
        let context = ResolverContext::new(
            workspace.path("home"),
            workspace.path("home/.config/loadout/loadout.yaml"),
            workspace.path("config/config.yaml"),
            workspace.path("state"),
        )
        .unwrap();
        let store_environment = environment(Some("workstation"), &["../profiles"], "../store");

        let control_error = resolve(&context, &store_environment, None).unwrap_err();
        assert!(matches!(
            control_error,
            ResolverError::ProtectedTarget {
                protected_by: "runtime configuration directory",
                ..
            }
        ));

        workspace.write("home/store/git/config", "[user]\n");
        let store_target = workspace.path("home/store/git/managed");
        workspace.write(
            "profiles/workstation.yaml",
            &profile(
                "workstation",
                &[],
                &file_resource("git", "git/config", &store_target.to_string_lossy()),
            ),
        );
        let home_store_environment =
            environment(Some("workstation"), &["../profiles"], "../home/store");
        let store_error = resolve(&context, &home_store_environment, None).unwrap_err();
        assert!(matches!(
            store_error,
            ResolverError::ProtectedTarget {
                protected_by: "local store root",
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn resolver_rejects_targets_inside_physical_protected_paths_through_a_home_alias() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("physical-home/store/git/config", "[user]\nname = Example\n");
        workspace.write("physical-home/runtime/loadout.yaml", "schema_version: 1\n");
        workspace.write(
            "physical-home/environment/config.yaml",
            "schema_version: 1\n",
        );
        workspace.create_dir("physical-home/state");
        workspace.write(
            "physical-home/profiles/workstation.yaml",
            &profile("workstation", &[], ""),
        );
        symlink(
            workspace.path("physical-home"),
            workspace.path("declared-home"),
        )
        .unwrap();
        let context = ResolverContext::new(
            workspace.path("declared-home"),
            workspace.path("declared-home/runtime/loadout.yaml"),
            workspace.path("declared-home/environment/config.yaml"),
            workspace.path("declared-home/state"),
        )
        .unwrap();
        let environment = environment(Some("workstation"), &["../profiles"], "../store");
        let source_before =
            fs::read_to_string(workspace.path("physical-home/store/git/config")).unwrap();
        let runtime_before =
            fs::read_to_string(workspace.path("physical-home/runtime/loadout.yaml")).unwrap();
        let environment_before =
            fs::read_to_string(workspace.path("physical-home/environment/config.yaml")).unwrap();

        for (target, protected_by) in [
            ("~/store/managed", "local store root"),
            ("~/runtime/managed", "runtime configuration directory"),
            (
                "~/environment/config.yaml",
                "environment configuration file",
            ),
            ("~/state/managed", "state directory"),
        ] {
            workspace.write(
                "physical-home/profiles/workstation.yaml",
                &profile(
                    "workstation",
                    &[],
                    &file_resource("managed", "git/config", target),
                ),
            );

            let error = resolve(&context, &environment, None).unwrap_err();

            match error {
                ResolverError::ProtectedTarget {
                    protected_by: actual,
                    ..
                } => assert_eq!(actual, protected_by),
                unexpected => panic!("unexpected resolver error: {unexpected:?}"),
            }
        }

        assert_eq!(
            fs::read_to_string(workspace.path("physical-home/store/git/config")).unwrap(),
            source_before
        );
        assert_eq!(
            fs::read_to_string(workspace.path("physical-home/runtime/loadout.yaml")).unwrap(),
            runtime_before
        );
        assert_eq!(
            fs::read_to_string(workspace.path("physical-home/environment/config.yaml")).unwrap(),
            environment_before
        );
        assert!(
            !workspace.path("physical-home/store/managed").exists(),
            "resolver must reject a target in the physical local store before any target mutation"
        );
    }

    #[test]
    fn resolver_validates_profile_resource_store_and_root_identifiers() {
        let workspace = TestWorkspace::new();
        workspace.write(
            "profiles/workstation.yaml",
            &profile("Workstation", &[], ""),
        );
        let context = workspace.context();
        let no_store_environment = environment_without_stores("workstation", &["../profiles"]);

        let profile_error = resolve(&context, &no_store_environment, None).unwrap_err();
        assert!(matches!(
            profile_error,
            ResolverError::InvalidIdentifier {
                role: "profile ID",
                ..
            }
        ));

        workspace.write("store/git/config", "[user]\n");
        workspace.write(
            "profiles/workstation.yaml",
            &profile(
                "workstation",
                &[],
                &file_resource("Git", "git/config", "~/.gitconfig"),
            ),
        );
        let store_environment = environment(Some("workstation"), &["../profiles"], "../store");
        let resource_error = resolve(&context, &store_environment, None).unwrap_err();
        assert!(matches!(
            resource_error,
            ResolverError::InvalidIdentifier {
                role: "resource ID",
                ..
            }
        ));

        workspace.write(
            "profiles/workstation.yaml",
            &profile(
                "workstation",
                &[],
                &file_resource("git", "git/config", "~/.gitconfig"),
            ),
        );
        let missing_store_error = resolve(&context, &no_store_environment, None).unwrap_err();
        assert!(matches!(
            missing_store_error,
            ResolverError::MissingStore { .. }
        ));

        let absent_default = environment_without_stores("absent", &["../profiles"]);
        let default_error = resolve(&context, &absent_default, None).unwrap_err();
        assert!(matches!(
            default_error,
            ResolverError::RootProfileNotDiscovered { .. }
        ));
    }

    #[test]
    fn resolver_rejects_a_missing_local_store_root() {
        let workspace = TestWorkspace::new();
        workspace.write(
            "profiles/workstation.yaml",
            &profile("workstation", &[], ""),
        );
        let context = workspace.context();
        let environment = environment(Some("workstation"), &["../profiles"], "../missing-store");

        let error = resolve(&context, &environment, None).unwrap_err();

        assert!(matches!(
            error,
            ResolverError::StoreVerification {
                source: SourceVerificationError::StoreRootIo { .. },
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn resolver_does_not_follow_a_symlinked_profile_file_during_discovery() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("outside/workstation.yaml", &profile("workstation", &[], ""));
        workspace.create_dir("profiles");
        symlink(
            workspace.path("outside/workstation.yaml"),
            workspace.path("profiles/workstation.yaml"),
        )
        .unwrap();
        let context = workspace.context();
        let environment = environment_without_stores("workstation", &["../profiles"]);

        let error = resolve(&context, &environment, None).unwrap_err();

        assert!(matches!(
            error,
            ResolverError::RootProfileNotDiscovered { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn resolver_rejects_a_symlinked_profile_discovery_directory() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("outside/workstation.yaml", &profile("workstation", &[], ""));
        symlink(workspace.path("outside"), workspace.path("profiles-link")).unwrap();
        let context = workspace.context();
        let environment = environment_without_stores("workstation", &["../profiles-link"]);

        let error = resolve(&context, &environment, None).unwrap_err();

        assert!(matches!(
            error,
            ResolverError::ProfileDiscoveryLinkOrReparsePoint { .. }
        ));
    }
}
