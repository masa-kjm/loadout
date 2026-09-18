//! Read-only application requests. Control-path discovery belongs to the loader.

use std::{fs, io};

use crate::declaration::environment_config::{EnvironmentConfig, EnvironmentConfigError};
use crate::domain::actual::ActualFileLink;
use crate::domain::ids::{FullyQualifiedResourceId, ProfileId};
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
        resolver::resolve(context, &configuration, Some(default_profile))
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

pub(crate) fn validate(request: &ValidationRequest) -> Result<ValidationReport, QueryError> {
    let environment = load_environment(&request.context)?;
    let roots = match &request.selection {
        ValidationSelection::Root(root) => {
            let resolved = resolver::resolve(&request.context, &environment, root.as_deref())
                .map_err(QueryError::Resolution)?;
            return Ok(ValidationReport {
                profiles: vec![(resolved.root_profile().clone(), Ok(()))],
            });
        }
        ValidationSelection::All => resolver::discovered_roots(&request.context, &environment)
            .map_err(QueryError::Resolution)?,
    };
    let profiles = roots
        .into_iter()
        .map(|root| {
            let result =
                resolver::resolve(&request.context, &environment, Some(root.as_str())).map(|_| ());
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
