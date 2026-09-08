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
pub(crate) enum QueryError {
    ConfigurationRead {
        path: crate::domain::paths::ResolvedPath,
        source: io::Error,
    },
    Configuration(EnvironmentConfigError),
    Resolution(ResolverError),
    State(StateRepositoryError),
    Inspection(TargetInspectionError),
}

fn load_environment(context: &ResolverContext) -> Result<EnvironmentConfig, QueryError> {
    let yaml =
        fs::read_to_string(context.environment_config_path().as_ref()).map_err(|source| {
            QueryError::ConfigurationRead {
                path: context.environment_config_path().clone(),
                source,
            }
        })?;
    EnvironmentConfig::parse(&yaml).map_err(QueryError::Configuration)
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
