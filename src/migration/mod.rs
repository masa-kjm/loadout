//! Explicit schema-3 to schema-4 document conversion.
//!
//! This module is the only crate boundary that parses legacy documents.
//! Normal declaration and state readers deliberately accept schema 4 only.

#![allow(dead_code)] // The explicit authoring adapter consumes this facade in W4.

use std::fmt;

use crate::resolver::ResolverContext;
use crate::state::repository::StateRepository;

/// The one explicit operation selected by the future authoring adapter.
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
                    source_schema_version: 3,
                    target_schema_version: 4,
                }]
            }
            MigrationOperation::Profiles => {
                let paths = schema_3_to_4::profile_migration_document_paths(&request.context)
                    .map_err(MigrationFacadeError::Migration)?;
                paths
                    .into_iter()
                    .map(|path| MigrationDocument {
                        path: path.display().to_string(),
                        source_schema_version: 3,
                        target_schema_version: 4,
                    })
                    .collect()
            }
        };
        Ok(MigrationPresentation {
            operation: request.operation,
            documents,
            source_schema_version: 3,
            target_schema_version: 4,
            required_next_step: match request.operation {
                MigrationOperation::State => {
                    "migrate profiles after the schema-4 state barrier is present"
                }
                MigrationOperation::Profiles => {
                    "resume only remaining schema-3 profiles if publication was interrupted"
                }
            },
            ownership_invariant: "resource IDs, effects, and Known ownership do not change",
        })
    }

    /// Selects only the registered 3-to-4 route.
    pub(crate) fn execute(
        request: MigrationRequest,
    ) -> Result<MigrationOutcome, MigrationFacadeError> {
        match request.operation {
            MigrationOperation::State => {
                let repository = StateRepository::new(request.context.state_directory().clone());
                schema_3_to_4::migrate_state(&repository, &request.context)
                    .map_err(MigrationFacadeError::State)?;
            }
            MigrationOperation::Profiles => {
                let repository = StateRepository::new(request.context.state_directory().clone());
                schema_3_to_4::migrate_profiles(&repository, &request.context)
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
    Migration(schema_3_to_4::MigrationError),
    State(schema_3_to_4::MigrationExecutionError),
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

// Route-private parsing, validation, conversion, inventory, and publication logic deliberately
// live in this named version-pair unit. The facade above is the route's only stable edge.
mod schema_3_to_4;

#[cfg(test)]
pub(crate) use schema_3_to_4::{
    ProfilePublicationFaultPoint, fail_profile_publication_at_for_test,
};
