//! State-specific validation before migration publication.

use super::super::{schema_3_to_4, schema_4_to_5, schema_5_to_6};
use super::{MigrationPlan, PlanError, PlannedDocument, Route, select_routes};
use crate::resolver::ResolverContext;
use crate::state::repository::StateRepository;
use crate::state::validate_current_state_bytes;
use serde_json::Value as JsonValue;
use std::path::PathBuf;

pub(super) fn publish(
    plan: &MigrationPlan,
    context: &ResolverContext,
    document: &PlannedDocument,
) -> Result<(), PlanError> {
    let repository = StateRepository::new(context.state_directory().clone());
    let locked = repository
        .acquire_exclusive_for_migration()
        .map_err(|error| PlanError::Invalid(error.to_string()))?;
    plan.compare_confirmed(context)?;
    if locked
        .read_source()
        .map_err(|error| PlanError::Invalid(error.to_string()))?
        != document.source
    {
        return Err(PlanError::ConfirmedSnapshotChanged);
    }
    locked
        .publish_migration_candidate(&document.source, &document.candidate, |bytes| {
            validate_state(document.target_generation, bytes).map_err(|error| error.to_string())
        })
        .map_err(|error| PlanError::Invalid(error.to_string()))?;
    if super::read_regular(&document.path)? != document.candidate {
        return Err(PlanError::ConfirmedSnapshotChanged);
    }
    validate_state(document.target_generation, &document.candidate)
}

pub(super) fn plan_document(
    path: PathBuf,
    source: Vec<u8>,
    from: u32,
    target: u32,
    registry: &[Route],
) -> Result<PlannedDocument, PlanError> {
    let route = select_routes(registry, from, target)?;
    let mut candidate = source.clone();
    for edge in &route {
        candidate = (edge.state)(&candidate).map_err(PlanError::Invalid)?;
        validate_state(edge.to, &candidate)?;
    }
    if route.is_empty() {
        validate_state(from, &candidate)?;
    }
    Ok(PlannedDocument {
        path,
        source_generation: from,
        target_generation: target,
        route: route.iter().map(|edge| (edge.from, edge.to)).collect(),
        source,
        candidate,
    })
}

pub(super) fn state_version(source: &[u8]) -> Result<u32, PlanError> {
    let document: JsonValue =
        serde_json::from_slice(source).map_err(|error| PlanError::Invalid(error.to_string()))?;
    document["schema_version"]
        .as_u64()
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| PlanError::Invalid("state has no valid schema_version".to_owned()))
}

pub(super) fn validate_state(version: u32, source: &[u8]) -> Result<(), PlanError> {
    match version {
        3 => schema_3_to_4::convert_state(source)
            .map(|_| ())
            .map_err(|error| PlanError::Invalid(error.to_string())),
        4 => schema_4_to_5::validate_schema_four_state(source)
            .map_err(|error| PlanError::Invalid(error.to_string())),
        5 => schema_5_to_6::state::validate_schema_five_state(source)
            .map_err(|error| PlanError::Invalid(error.to_string())),
        6 => {
            validate_current_state_bytes(source)
                .map_err(|error| PlanError::Invalid(error.to_string()))?;
            let value: JsonValue = serde_json::from_slice(source)
                .map_err(|error| PlanError::Invalid(error.to_string()))?;
            if !value["active_operation"].is_null() {
                return Err(PlanError::Invalid(
                    "state has an active operation".to_owned(),
                ));
            }
            Ok(())
        }
        _ => Err(PlanError::UnsupportedGeneration {
            from: version,
            target: 6,
        }),
    }
}
