//! Explicit managed-document migration and staged continuous-route planning.
//!
//! This module is the only crate boundary that parses legacy documents.
//! Normal declaration and state readers deliberately accept schema 5 only.

use std::fmt;

use crate::resolver::ResolverContext;
use crate::state::repository::StateRepository;

/// One legacy public operation retained until the unified command is introduced.
///
/// This is data only: it carries no parsed legacy document, inventory, lock, writer, or publication capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MigrationOperation {
    State,
    Profiles,
}

/// Input accepted by the sole crate-private migration edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MigrationRequest {
    operation: MigrationOperation,
    context: ResolverContext,
}

impl MigrationRequest {
    pub(crate) fn new(operation: MigrationOperation, context: ResolverContext) -> Self {
        Self { operation, context }
    }
}

/// Data-only information an adapter may present before it obtains confirmation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MigrationPresentation {
    pub(crate) operation: MigrationOperation,
    pub(crate) documents: Vec<MigrationDocument>,
    pub(crate) source_schema_version: u32,
    pub(crate) target_schema_version: u32,
    pub(crate) required_next_step: &'static str,
    pub(crate) ownership_invariant: &'static str,
}

/// A data-only document description shown before confirmation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MigrationDocument {
    pub(crate) path: String,
    pub(crate) source_schema_version: u32,
    pub(crate) target_schema_version: u32,
}

/// Data-only result returned after one selected migration operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MigrationOutcome {
    pub(crate) operation: MigrationOperation,
}

/// The sole stable migration edge outside this module.
pub(crate) struct MigrationFacade;

impl MigrationFacade {
    /// Builds the complete continuous-route plan without publishing a document.
    #[allow(dead_code)] // The new command adapter consumes this entry after publication support is added.
    fn plan_continuous(
        context: &ResolverContext,
        target: u32,
    ) -> Result<continuous::MigrationPlan, continuous::PlanError> {
        continuous::MigrationPlan::build(context, target)
    }

    pub(crate) fn present(
        request: &MigrationRequest,
    ) -> Result<MigrationPresentation, MigrationFacadeError> {
        let documents = match request.operation {
            MigrationOperation::State => {
                vec![MigrationDocument {
                    path: request
                        .context
                        .state_directory()
                        .as_path()
                        .join("state.json")
                        .display()
                        .to_string(),
                    source_schema_version: 4,
                    target_schema_version: 5,
                }]
            }
            MigrationOperation::Profiles => {
                let paths = schema_4_to_5::profile_migration_document_paths(&request.context)
                    .map_err(MigrationFacadeError::Migration)?;
                paths
                    .into_iter()
                    .map(|path| MigrationDocument {
                        path: path.display().to_string(),
                        source_schema_version: 4,
                        target_schema_version: 5,
                    })
                    .collect()
            }
        };
        Ok(MigrationPresentation {
            operation: request.operation,
            documents,
            source_schema_version: 4,
            target_schema_version: 5,
            required_next_step: match request.operation {
                MigrationOperation::State => {
                    "migrate profiles after the schema-5 state barrier is present"
                }
                MigrationOperation::Profiles => {
                    "resume only remaining schema-4 profiles if publication was interrupted"
                }
            },
            ownership_invariant: "resource IDs, effects, and Known ownership do not change",
        })
    }

    /// Selects only the registered 4-to-5 route.
    pub(crate) fn execute(
        request: MigrationRequest,
    ) -> Result<MigrationOutcome, MigrationFacadeError> {
        match request.operation {
            MigrationOperation::State => {
                let repository = StateRepository::new(request.context.state_directory().clone());
                schema_4_to_5::migrate_state(&repository, &request.context)
                    .map_err(MigrationFacadeError::State)?;
            }
            MigrationOperation::Profiles => {
                let repository = StateRepository::new(request.context.state_directory().clone());
                schema_4_to_5::migrate_profiles(&repository, &request.context)
                    .map_err(MigrationFacadeError::State)?;
            }
        }
        Ok(MigrationOutcome {
            operation: request.operation,
        })
    }
}

/// An adapter-visible failure that contains no route-private data or capability.
#[derive(Debug)]
pub(crate) enum MigrationFacadeError {
    Migration(schema_4_to_5::MigrationError),
    State(schema_4_to_5::MigrationExecutionError),
}

impl fmt::Display for MigrationFacadeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Migration(error) => error.fmt(formatter),
            Self::State(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MigrationFacadeError {}

// The schema-3 route remains private and conversion-only until publication support is added.
mod continuous;
mod schema_3_to_4;

// The existing 4-to-5 route retains the old command's publication path.
// Continuous planning selects private routes through the facade.
mod schema_4_to_5;

#[cfg(test)]
pub(crate) use schema_4_to_5::{
    ProfilePublicationFaultPoint, fail_profile_publication_at_for_test,
};
