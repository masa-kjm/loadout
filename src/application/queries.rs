//! Read-only application requests. Control-path discovery belongs to the loader.

use std::{fs, io};

use crate::declaration::environment_config::{EnvironmentConfig, EnvironmentConfigError};
use crate::domain::actual::ActualFileLink;
use crate::domain::desired::ResolvedDesired;
use crate::domain::file_link::ResolvedFileLink;
use crate::domain::ids::{FullyQualifiedResourceId, ProfileId};
use crate::domain::known::KnownFileLink;
use crate::domain::plan::Plan;
use crate::inspection::file_link::{FileLinkInspector, TargetInspectionError};
use crate::planner::file_link::plan;
use crate::resolver::{self, ResolvedApplyInput, ResolverContext, ResolverError};
use crate::state::operation::OperationRecord;
use crate::state::repository::{StateRepository, StateRepositoryError};

/// Selected control paths and an optional root; declarations remain unread.
pub(crate) struct DeclarationRequest {
    pub(crate) context: ResolverContext,
    pub(crate) root: Option<String>,
}

pub(crate) enum ValidationSelection {
    Root(Option<String>),
    All,
}

pub(crate) struct ValidationRequest {
    pub(crate) context: ResolverContext,
    pub(crate) selection: ValidationSelection,
}

#[derive(Debug)]
pub(crate) struct ValidationReport {
    pub(crate) profiles: Vec<(ProfileId, Result<(), ResolverError>)>,
}

#[derive(Debug)]
pub(crate) struct DiffReport {
    pub(crate) resources: Vec<(FullyQualifiedResourceId, ActualFileLink)>,
    pub(crate) active_operation: Option<OperationRecord>,
}

#[derive(Debug)]
pub(crate) struct PlanReport {
    pub(crate) profile: ProfileId,
    pub(crate) plan: Plan,
}

/// Strictly parsed profile declarations for read-only presentation.
#[derive(Debug)]
pub(crate) struct ProfileListReport {
    pub(crate) profiles: Vec<resolver::DeclaredProfile>,
}

/// One selected strictly parsed profile declaration.
#[derive(Debug)]
pub(crate) struct ProfileShowReport {
    pub(crate) profile: resolver::DeclaredProfile,
}

/// One selected root profile's canonical Desired resources.
#[derive(Debug)]
pub(crate) struct DesiredResourcesReport {
    pub(crate) root_profile: ProfileId,
    pub(crate) resources: Vec<ResolvedFileLink>,
}

/// Validated historical resource records without target observation.
#[derive(Debug)]
pub(crate) struct KnownResourcesReport {
    pub(crate) resources: Vec<KnownFileLink>,
}

/// A status report that keeps Desired/Known association separate from Actual observation.
#[derive(Debug)]
pub(crate) struct StatusReport {
    pub(crate) active_operation: Option<OperationRecord>,
    pub(crate) desired: StatusDesired,
    pub(crate) has_unavailable_observation: bool,
    pub(crate) inspection_initialization_error: Option<TargetInspectionError>,
}

/// The declaration portion of a status report.
#[derive(Debug)]
pub(crate) enum StatusDesired {
    Available {
        root_profile: ProfileId,
        resources: Vec<StatusResource>,
    },
    Unavailable(QueryError),
}

/// One identity in the Desired/Known union and its independent Actual observation.
#[derive(Debug)]
pub(crate) struct StatusResource {
    pub(crate) resource_id: FullyQualifiedResourceId,
    pub(crate) desired: Option<ResolvedFileLink>,
    pub(crate) known: Option<KnownFileLink>,
    pub(crate) actual: StatusActual,
}

/// One Actual observation or the error that prevented its establishment.
#[derive(Debug)]
pub(crate) enum StatusActual {
    Available(ActualFileLink),
    Unavailable(StatusUnavailable),
}

/// The typed reason an Actual observation is unavailable.
#[derive(Debug)]
pub(crate) enum StatusUnavailable {
    InspectorInitialization,
    Observation(TargetInspectionError),
}

/// The identity and definition relation before interpreting its Actual observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StatusRelationship {
    DesiredOnly,
    KnownOnly,
    DefinitionChanged,
    DefinitionsMatch,
}

impl StatusResource {
    /// Classifies Desired/Known association without deriving an action or ownership decision.
    pub(crate) fn relationship(&self) -> StatusRelationship {
        match (&self.desired, &self.known) {
            (Some(_), None) => StatusRelationship::DesiredOnly,
            (None, Some(_)) => StatusRelationship::KnownOnly,
            (Some(desired), Some(known))
                if desired.source_path() == known.source_path()
                    && desired.target_path() == known.target_path()
                    && desired.link_target() == known.link_target() =>
            {
                StatusRelationship::DefinitionsMatch
            }
            (Some(_), Some(_)) => StatusRelationship::DefinitionChanged,
            (None, None) => unreachable!("a status resource has Desired or Known facts"),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ConfigurationReport {
    pub(crate) configuration_path: crate::domain::paths::ResolvedPath,
    pub(crate) default_profile: Option<String>,
    pub(crate) stores: Vec<(String, crate::domain::paths::ResolvedPath)>,
}

#[derive(Debug)]
pub(crate) enum ConfigurationValue {
    DefaultProfile(Option<String>),
    StorePath(crate::domain::paths::ResolvedPath),
}

#[derive(Debug)]
pub(crate) struct ConfigurationValueReport {
    pub(crate) configuration_path: crate::domain::paths::ResolvedPath,
    pub(crate) value: ConfigurationValue,
}

#[derive(Debug)]
pub(crate) enum QueryError {
    ConfigurationRead {
        path: crate::domain::paths::ResolvedPath,
        source: io::Error,
    },
    Configuration(EnvironmentConfigError),
    Resolution(ResolverError),
    State(StateRepositoryError),
    Inspection(TargetInspectionError),
    ConfigField(String),
}

fn load_environment(context: &ResolverContext) -> Result<EnvironmentConfig, QueryError> {
    #[cfg(test)]
    crate::test_support::assert_desired_dependencies_allowed();
    let yaml =
        fs::read_to_string(context.environment_config_path().as_ref()).map_err(|source| {
            QueryError::ConfigurationRead {
                path: context.environment_config_path().clone(),
                source,
            }
        })?;
    EnvironmentConfig::parse(&yaml).map_err(QueryError::Configuration)
}

/// Reads and structurally validates the selected portable configuration without lifecycle access.
pub(crate) fn configuration(context: &ResolverContext) -> Result<EnvironmentConfig, QueryError> {
    let configuration = load_environment(context)?;
    validate_configuration(context, configuration)
}

/// Validates a complete candidate document without writing it.
pub(crate) fn configuration_candidate(
    context: &ResolverContext,
    yaml: &str,
) -> Result<EnvironmentConfig, QueryError> {
    let configuration = EnvironmentConfig::parse(yaml).map_err(QueryError::Configuration)?;
    validate_configuration(context, configuration)
}

fn validate_configuration(
    context: &ResolverContext,
    configuration: EnvironmentConfig,
) -> Result<EnvironmentConfig, QueryError> {
    resolver::resolved_store_paths(context, &configuration).map_err(QueryError::Resolution)?;
    resolver::discovered_roots(context, &configuration).map_err(QueryError::Resolution)?;
    if let Some(default_profile) = configuration.default_profile() {
        resolver::resolve_for_apply(context, &configuration, Some(default_profile))
            .map_err(QueryError::Resolution)?;
    }
    Ok(configuration)
}

pub(crate) fn configuration_report(
    context: &ResolverContext,
) -> Result<ConfigurationReport, QueryError> {
    let configuration = configuration(context)?;
    Ok(ConfigurationReport {
        configuration_path: context.environment_config_path().clone(),
        default_profile: configuration.default_profile().map(str::to_owned),
        stores: resolver::resolved_store_paths(context, &configuration)
            .map_err(QueryError::Resolution)?,
    })
}

pub(crate) fn configuration_value(
    context: &ResolverContext,
    field: &str,
) -> Result<ConfigurationValueReport, QueryError> {
    let configuration = configuration(context)?;
    if field == "default_profile" {
        return Ok(ConfigurationValueReport {
            configuration_path: context.environment_config_path().clone(),
            value: ConfigurationValue::DefaultProfile(
                configuration.default_profile().map(str::to_owned),
            ),
        });
    }
    let Some(store_id) = field
        .strip_prefix("stores.")
        .and_then(|tail| tail.strip_suffix(".properties.path"))
    else {
        return Err(QueryError::ConfigField(field.into()));
    };
    let Some((_, path)) = resolver::resolved_store_paths(context, &configuration)
        .map_err(QueryError::Resolution)?
        .into_iter()
        .find(|(id, _)| id == store_id)
    else {
        return Err(QueryError::ConfigField(field.into()));
    };
    Ok(ConfigurationValueReport {
        configuration_path: context.environment_config_path().clone(),
        value: ConfigurationValue::StorePath(path),
    })
}

pub(super) fn resolve_request(
    request: &DeclarationRequest,
) -> Result<ResolvedApplyInput, QueryError> {
    let environment = load_environment(&request.context)?;
    resolver::resolve_for_apply(&request.context, &environment, request.root.as_deref())
        .map_err(QueryError::Resolution)
}

fn resolve_desired(request: &DeclarationRequest) -> Result<ResolvedDesired, QueryError> {
    let environment = load_environment(&request.context)?;
    resolver::resolve(&request.context, &environment, request.root.as_deref())
        .map_err(QueryError::Resolution)
}

/// Discovers strictly parsed profile declarations without lifecycle or state access.
pub(crate) fn profiles(context: &ResolverContext) -> Result<ProfileListReport, QueryError> {
    let environment = load_environment(context)?;
    Ok(ProfileListReport {
        profiles: resolver::declared_profiles(context, &environment)
            .map_err(QueryError::Resolution)?,
    })
}

/// Finds one strictly parsed profile declaration by its validated identity.
pub(crate) fn profile(
    context: &ResolverContext,
    profile_id: &ProfileId,
) -> Result<Option<ProfileShowReport>, QueryError> {
    Ok(profiles(context)?
        .profiles
        .into_iter()
        .find_map(|profile| (profile.id() == profile_id).then_some(ProfileShowReport { profile })))
}

/// Resolves Desired resources without state or managed-target observation.
pub(crate) fn desired_resources(
    request: &DeclarationRequest,
) -> Result<DesiredResourcesReport, QueryError> {
    let desired = resolve_desired(request)?;
    Ok(DesiredResourcesReport {
        root_profile: desired.root_profile().clone(),
        resources: desired.resources().to_vec(),
    })
}

/// Reads validated Known resources without declaration or target access.
pub(crate) fn known_resources(
    repository: &StateRepository,
) -> Result<KnownResourcesReport, QueryError> {
    let state = repository.load().map_err(QueryError::State)?;
    Ok(KnownResourcesReport {
        resources: state.known().resources().cloned().collect(),
    })
}

/// Reports Desired, Known, and Actual facts without planning, recovery, or mutation.
pub(crate) fn status(
    request: &DeclarationRequest,
    repository: &StateRepository,
) -> Result<StatusReport, QueryError> {
    status_with_inspector(request, repository, FileLinkInspector::new)
}

fn status_with_inspector(
    request: &DeclarationRequest,
    repository: &StateRepository,
    initialize_inspector: impl FnOnce(
        &std::path::Path,
    ) -> Result<FileLinkInspector, TargetInspectionError>,
) -> Result<StatusReport, QueryError> {
    let state = repository.load().map_err(QueryError::State)?;
    let active_operation = state.active_operation().cloned();
    let desired = match resolve_desired(request) {
        Ok(desired) => desired,
        Err(error) => {
            return Ok(StatusReport {
                active_operation,
                desired: StatusDesired::Unavailable(error),
                has_unavailable_observation: false,
                inspection_initialization_error: None,
            });
        }
    };
    let (inspector, inspection_initialization_error) =
        match initialize_inspector(request.context.home_directory().as_ref()) {
            Ok(inspector) => (Some(inspector), None),
            Err(error) => (None, Some(error)),
        };
    let mut desired_by_id = desired
        .resources()
        .iter()
        .cloned()
        .map(|resource| (resource.resource_id().clone(), resource))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut known_by_id = state
        .known()
        .resources()
        .cloned()
        .map(|resource| (resource.resource_id().clone(), resource))
        .collect::<std::collections::BTreeMap<_, _>>();
    let resource_ids = desired_by_id
        .keys()
        .chain(known_by_id.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut resources = Vec::new();
    let mut has_unavailable_observation = false;
    for resource_id in resource_ids {
        let desired = desired_by_id.remove(&resource_id);
        let known = known_by_id.remove(&resource_id);
        let (target_path, link_target) = match (&known, &desired) {
            (Some(known), _) => (known.target_path(), known.link_target()),
            (None, Some(desired)) => (desired.target_path(), desired.link_target()),
            (None, None) => unreachable!("the union key has one source"),
        };
        let actual = if let Some(inspector) = &inspector {
            let observation = match known.is_some() {
                true => inspector.inspect_target_for_expected_link(target_path, link_target),
                false => inspector.inspect_target_for_desired_link(target_path, link_target),
            };
            match observation {
                Ok(actual) => StatusActual::Available(actual),
                Err(error) => {
                    has_unavailable_observation = true;
                    StatusActual::Unavailable(StatusUnavailable::Observation(error))
                }
            }
        } else {
            has_unavailable_observation = true;
            StatusActual::Unavailable(StatusUnavailable::InspectorInitialization)
        };
        resources.push(StatusResource {
            resource_id,
            desired,
            known,
            actual,
        });
    }
    Ok(StatusReport {
        active_operation,
        desired: StatusDesired::Available {
            root_profile: desired.root_profile().clone(),
            resources,
        },
        has_unavailable_observation,
        inspection_initialization_error,
    })
}

pub(crate) fn validate(request: &ValidationRequest) -> Result<ValidationReport, QueryError> {
    let environment = load_environment(&request.context)?;
    let roots = match &request.selection {
        ValidationSelection::Root(root) => {
            let resolved =
                resolver::resolve_for_apply(&request.context, &environment, root.as_deref())
                    .map_err(QueryError::Resolution)?;
            return Ok(ValidationReport {
                profiles: vec![(resolved.desired().root_profile().clone(), Ok(()))],
            });
        }
        ValidationSelection::All => resolver::discovered_roots(&request.context, &environment)
            .map_err(QueryError::Resolution)?,
    };
    let profiles = roots
        .into_iter()
        .map(|root| {
            let result =
                resolver::resolve_for_apply(&request.context, &environment, Some(root.as_str()))
                    .map(|_| ());
            (root, result)
        })
        .collect();
    Ok(ValidationReport { profiles })
}

/// Has no declaration or source dependency and never reconciles operations.
pub(crate) fn diff(
    home: &crate::domain::paths::ResolvedPath,
    repository: &StateRepository,
) -> Result<DiffReport, QueryError> {
    let state = repository.load().map_err(QueryError::State)?;
    let mut resources = Vec::new();
    if state.known().resources().len() != 0 {
        let inspector = FileLinkInspector::new(home.as_ref()).map_err(QueryError::Inspection)?;
        for resource in state.known().resources() {
            let actual = inspector
                .inspect_target_for_expected_link(resource.target_path(), resource.link_target())
                .map_err(QueryError::Inspection)?;
            resources.push((resource.resource_id().clone(), actual));
        }
    }
    Ok(DiffReport {
        resources,
        active_operation: state.active_operation().cloned(),
    })
}

/// Shared plan/dry-run path: no exclusive lock, recovery, or mutation preflight.
pub(crate) fn plan_request(request: &DeclarationRequest) -> Result<PlanReport, QueryError> {
    let resolved = resolve_request(request)?;
    let state = StateRepository::new(request.context.state_directory().clone())
        .load()
        .map_err(QueryError::State)?;
    let inspector = FileLinkInspector::new(request.context.home_directory().as_ref())
        .map_err(QueryError::Inspection)?;
    let actual = inspector
        .inspect(resolved.desired(), state.known())
        .map_err(QueryError::Inspection)?;
    Ok(PlanReport {
        profile: resolved.desired().root_profile().clone(),
        plan: plan(resolved.desired(), state.known(), &actual),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        forbid_desired_dependencies, forbid_mutation, forbid_target_inspection,
    };
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

    struct Workspace {
        root: std::path::PathBuf,
    }

    impl Workspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loadout-queries-{}-{}",
                std::process::id(),
                NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            for directory in ["home", "portable/profiles", "store", "state"] {
                fs::create_dir_all(root.join(directory)).unwrap();
            }
            fs::write(root.join("store/source"), "source\n").unwrap();
            fs::write(
                root.join("portable/config.yaml"),
                "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: ../store\n",
            )
            .unwrap();
            fs::write(
                root.join("portable/profiles/base.yaml"),
                "schema_version: 1\nid: base\nresources:\n  item:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: ~/.item\n",
            )
            .unwrap();
            let root = crate::domain::paths::ResolvedPath::from_platform_canonicalized(
                fs::canonicalize(root).unwrap(),
            )
            .unwrap()
            .into_path_buf();
            Self { root }
        }

        fn context(&self) -> ResolverContext {
            ResolverContext::new(
                self.root.join("home"),
                self.root.join("runtime/loadout.yaml"),
                self.root.join("portable/config.yaml"),
                self.root.join("state"),
            )
            .unwrap()
        }

        fn repository(&self) -> StateRepository {
            StateRepository::new(self.context().state_directory().clone())
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn persisted_known_file_link(resource: &ResolvedFileLink) -> serde_json::Value {
        serde_json::json!({
            "definition_hash": crate::domain::hashes::definition_hash(resource).unwrap().as_str(),
            "file_link": {
                "source_path": resource.source_path().as_ref(),
                "target_path": resource.target_path().as_ref(),
                "link_target": resource.link_target().as_path().as_ref(),
            },
        })
    }

    #[test]
    fn profile_and_desired_queries_do_not_inspect_targets_or_mutate() {
        let workspace = Workspace::new();
        let context = workspace.context();
        let _no_mutation = forbid_mutation();
        let _no_target_inspection = forbid_target_inspection();

        let profiles = profiles(&context).unwrap();
        assert_eq!(profiles.profiles.len(), 1);
        assert_eq!(profiles.profiles[0].id().as_str(), "base");
        assert_eq!(
            profiles.profiles[0].resources()[0].resource_id().as_str(),
            "item"
        );

        let desired = desired_resources(&DeclarationRequest {
            context,
            root: None,
        })
        .unwrap();
        assert_eq!(desired.root_profile.as_str(), "base");
        assert_eq!(desired.resources[0].resource_id().as_str(), "base/item");
    }

    #[cfg(windows)]
    #[test]
    fn workspace_root_uses_the_canonical_windows_spelling() {
        let workspace = Workspace::new();

        assert_eq!(
            crate::domain::paths::ResolvedPath::new(workspace.root.clone()).unwrap(),
            crate::domain::paths::ResolvedPath::from_platform_canonicalized(
                fs::canonicalize(&workspace.root).unwrap()
            )
            .unwrap()
        );
    }

    #[test]
    fn desired_query_keeps_a_missing_final_source_as_a_resolved_declaration() {
        let workspace = Workspace::new();
        fs::remove_file(workspace.root.join("store/source")).unwrap();

        let report = desired_resources(&DeclarationRequest {
            context: workspace.context(),
            root: None,
        })
        .unwrap();

        assert_eq!(report.resources.len(), 1);
        assert_eq!(
            report.resources[0].source_path().as_ref(),
            workspace.root.join("store/source")
        );
    }

    #[test]
    fn validate_retains_final_source_verification() {
        let workspace = Workspace::new();
        fs::remove_file(workspace.root.join("store/source")).unwrap();

        assert!(matches!(
            validate(&ValidationRequest {
                context: workspace.context(),
                selection: ValidationSelection::Root(None),
            }),
            Err(QueryError::Resolution(
                ResolverError::SourceVerification { .. }
            ))
        ));
    }

    #[test]
    fn complete_configuration_validation_retains_final_source_verification() {
        let workspace = Workspace::new();
        fs::remove_file(workspace.root.join("store/source")).unwrap();

        assert!(matches!(
            configuration(&workspace.context()),
            Err(QueryError::Resolution(
                ResolverError::SourceVerification { .. }
            ))
        ));
    }

    #[test]
    fn known_query_has_no_declaration_or_target_dependency() {
        let workspace = Workspace::new();
        let _no_mutation = forbid_mutation();
        let _no_target_inspection = forbid_target_inspection();
        let _no_declaration = forbid_desired_dependencies();

        let report = known_resources(&workspace.repository()).unwrap();
        assert!(report.resources.is_empty());
        assert!(!workspace.root.join("state/state.lock").exists());
    }

    #[test]
    fn status_reports_desired_only_with_a_non_ownership_observation() {
        let workspace = Workspace::new();
        let _no_mutation = forbid_mutation();
        let report = status(
            &DeclarationRequest {
                context: workspace.context(),
                root: None,
            },
            &workspace.repository(),
        )
        .unwrap();

        let StatusDesired::Available {
            root_profile,
            resources,
        } = report.desired
        else {
            panic!("expected resolved Desired status report");
        };
        assert_eq!(root_profile.as_str(), "base");
        assert!(report.active_operation.is_none());
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].relationship(), StatusRelationship::DesiredOnly);
        assert!(matches!(
            resources[0].actual,
            StatusActual::Available(ref actual)
                if matches!(actual.observation(),
            crate::domain::actual::TargetObservation::Missing
                )
        ));
        assert!(!workspace.root.join("state/state.lock").exists());
    }

    #[test]
    fn status_retains_active_operation_and_skips_target_observation_when_desired_is_unavailable() {
        let workspace = Workspace::new();
        fs::write(
            workspace.root.join("state/state.json"),
            serde_json::json!({
                "schema_version": 1,
                "resources": {},
                "active_operation": {
                    "id": "interrupted-operation",
                    "desired_hash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                    "actions": {
                        "a1": {
                            "kind": "create_link",
                            "resource_id": "base/pending",
                            "target_path": workspace.root.join("home/.pending"),
                            "precondition": { "target": "missing" },
                            "postcondition": {
                                "target": "expected_link",
                                "link_target": workspace.root.join("store/source"),
                            },
                            "status": "pending",
                        },
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            workspace.root.join("portable/profiles/base.yaml"),
            "not: a valid profile declaration\n",
        )
        .unwrap();
        let _no_target_inspection = forbid_target_inspection();

        let report = status(
            &DeclarationRequest {
                context: workspace.context(),
                root: None,
            },
            &workspace.repository(),
        )
        .unwrap();

        assert!(report.active_operation.is_some());
        assert!(matches!(report.desired, StatusDesired::Unavailable(_)));
        assert!(!report.has_unavailable_observation);
    }

    #[test]
    fn status_classifies_matching_changed_and_known_only_definitions() {
        let workspace = Workspace::new();
        fs::write(
            workspace.root.join("portable/profiles/base.yaml"),
            "schema_version: 1\nid: base\nresources:\n  changed:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: ~/.changed\n  item:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: ~/.item\n",
        )
        .unwrap();
        let desired_item = desired_resources(&DeclarationRequest {
            context: workspace.context(),
            root: None,
        })
        .unwrap()
        .resources
        .into_iter()
        .find(|resource| resource.resource_id().as_str() == "base/item")
        .unwrap();
        let mut resources = serde_json::Map::new();
        resources.insert(
            "base/item".to_owned(),
            persisted_known_file_link(&desired_item),
        );
        resources.insert(
            "base/changed".to_owned(),
            persisted_known_file_link(
                &ResolvedFileLink::new(
                    FullyQualifiedResourceId::parse("base/changed").unwrap(),
                    crate::domain::paths::ResolvedPath::new(
                        workspace.root.join("store/previous-source"),
                    )
                    .unwrap(),
                    crate::domain::paths::ResolvedPath::new(workspace.root.join("home/.changed"))
                        .unwrap(),
                )
                .unwrap(),
            ),
        );
        resources.insert(
            "base/legacy".to_owned(),
            persisted_known_file_link(
                &ResolvedFileLink::new(
                    FullyQualifiedResourceId::parse("base/legacy").unwrap(),
                    crate::domain::paths::ResolvedPath::new(workspace.root.join("store/source"))
                        .unwrap(),
                    crate::domain::paths::ResolvedPath::new(workspace.root.join("home/.legacy"))
                        .unwrap(),
                )
                .unwrap(),
            ),
        );
        fs::write(
            workspace.root.join("state/state.json"),
            serde_json::json!({
                "schema_version": 1,
                "resources": resources,
                "active_operation": null,
            })
            .to_string(),
        )
        .unwrap();

        let report = status(
            &DeclarationRequest {
                context: workspace.context(),
                root: None,
            },
            &workspace.repository(),
        )
        .unwrap();

        let StatusDesired::Available { resources, .. } = report.desired else {
            panic!("expected resolved Desired status report");
        };
        assert_eq!(resources.len(), 3);
        assert_eq!(resources[0].resource_id.as_str(), "base/changed");
        assert_eq!(
            resources[0].relationship(),
            StatusRelationship::DefinitionChanged
        );
        assert!(resources[0].desired.is_some());
        assert!(resources[0].known.is_some());
        assert_eq!(resources[1].resource_id.as_str(), "base/item");
        assert_eq!(
            resources[1].relationship(),
            StatusRelationship::DefinitionsMatch
        );
        assert!(resources[1].desired.is_some());
        assert!(resources[1].known.is_some());
        assert_eq!(resources[2].resource_id.as_str(), "base/legacy");
        assert_eq!(resources[2].relationship(), StatusRelationship::KnownOnly);
        assert!(resources[2].desired.is_none());
        assert!(resources[2].known.is_some());
        assert!(resources.iter().all(|resource| {
            matches!(
                resource.actual,
                StatusActual::Available(ref actual)
                    if matches!(actual.observation(), crate::domain::actual::TargetObservation::Missing)
            )
        }));
    }

    #[test]
    fn status_retains_desired_known_and_active_operation_when_inspector_initialization_fails() {
        let workspace = Workspace::new();
        let desired = desired_resources(&DeclarationRequest {
            context: workspace.context(),
            root: None,
        })
        .unwrap()
        .resources
        .pop()
        .unwrap();
        fs::write(
            workspace.root.join("state/state.json"),
            serde_json::json!({
                "schema_version": 1,
                "resources": {
                    "base/item": persisted_known_file_link(&desired),
                },
                "active_operation": {
                    "id": "interrupted-operation",
                    "desired_hash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                    "actions": {
                        "a1": {
                            "kind": "create_link",
                            "resource_id": "base/pending",
                            "target_path": workspace.root.join("home/.pending"),
                            "precondition": { "target": "missing" },
                            "postcondition": {
                                "target": "expected_link",
                                "link_target": workspace.root.join("store/source"),
                            },
                            "status": "pending",
                        },
                    },
                },
            })
            .to_string(),
        )
        .unwrap();

        let report = status_with_inspector(
            &DeclarationRequest {
                context: workspace.context(),
                root: None,
            },
            &workspace.repository(),
            |home_directory| {
                fs::remove_dir(home_directory).unwrap();
                fs::write(home_directory, "unavailable home directory\n").unwrap();
                FileLinkInspector::new(home_directory)
            },
        )
        .unwrap();

        assert!(report.active_operation.is_some());
        assert!(report.has_unavailable_observation);
        assert!(matches!(
            report.inspection_initialization_error,
            Some(TargetInspectionError::HomeDirectoryNotDirectory { .. })
        ));
        let StatusDesired::Available {
            root_profile,
            resources,
        } = report.desired
        else {
            panic!("expected resolved Desired status report");
        };
        assert_eq!(root_profile.as_str(), "base");
        assert_eq!(resources.len(), 1);
        assert!(resources[0].desired.is_some());
        assert!(resources[0].known.is_some());
        assert!(matches!(
            resources[0].actual,
            StatusActual::Unavailable(StatusUnavailable::InspectorInitialization)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn status_preserves_a_matching_desired_only_link_as_unmanaged() {
        use std::os::unix::fs::symlink;

        let workspace = Workspace::new();
        symlink(
            workspace.root.join("store/source"),
            workspace.root.join("home/.item"),
        )
        .unwrap();
        let report = status(
            &DeclarationRequest {
                context: workspace.context(),
                root: None,
            },
            &workspace.repository(),
        )
        .unwrap();

        let StatusDesired::Available { resources, .. } = report.desired else {
            panic!("expected resolved Desired status report");
        };
        assert!(matches!(
            resources[0].actual,
            StatusActual::Available(ref actual)
                if matches!(
                    actual.observation(),
                    crate::domain::actual::TargetObservation::MatchingUnmanagedLink { .. }
                )
        ));
    }

    #[cfg(unix)]
    #[test]
    fn status_retains_completed_rows_when_an_observation_is_unavailable() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = Workspace::new();
        fs::write(
            workspace.root.join("portable/profiles/base.yaml"),
            "schema_version: 1\nid: base\nresources:\n  another:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: ~/.another\n  item:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: ~/.blocked/item\n",
        )
        .unwrap();
        let blocked = workspace.root.join("home/.blocked");
        fs::create_dir(&blocked).unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
        let report = status(
            &DeclarationRequest {
                context: workspace.context(),
                root: None,
            },
            &workspace.repository(),
        )
        .unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();

        let StatusDesired::Available { resources, .. } = report.desired else {
            panic!("expected resolved Desired status report");
        };
        assert!(report.has_unavailable_observation);
        assert!(matches!(
            resources[0].actual,
            StatusActual::Available(ref actual)
                if matches!(actual.observation(), crate::domain::actual::TargetObservation::Missing)
        ));
        assert!(matches!(
            resources[1].actual,
            StatusActual::Unavailable(StatusUnavailable::Observation(_))
        ));
        assert!(report.active_operation.is_none());
    }

    #[test]
    fn invalid_state_prevents_status_target_inspection() {
        let workspace = Workspace::new();
        fs::write(workspace.root.join("state/state.json"), "not json").unwrap();
        let _no_mutation = forbid_mutation();
        let _no_target_inspection = forbid_target_inspection();

        assert!(matches!(
            status(
                &DeclarationRequest {
                    context: workspace.context(),
                    root: None,
                },
                &workspace.repository(),
            ),
            Err(QueryError::State(_))
        ));
    }
}
