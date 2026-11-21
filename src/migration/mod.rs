//! Explicit managed-document migration through one crate-private facade.

use std::fmt;

use crate::resolver::ResolverContext;

/// Data-only request for one continuous migration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MigrationRequest {
    context: ResolverContext,
    target_generation: u32,
}

impl MigrationRequest {
    pub(crate) fn new(context: ResolverContext, target_generation: u32) -> Self {
        Self {
            context,
            target_generation,
        }
    }
}

/// Data-only information an adapter may present before it obtains confirmation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MigrationPresentation {
    pub(crate) configuration_path: String,
    pub(crate) documents: Vec<MigrationDocument>,
    pub(crate) normal_commands_available: bool,
    pub(crate) ownership_invariant: &'static str,
    pub(crate) broadened_ordering: Vec<String>,
    snapshot_fingerprint: [u8; 32],
}

/// One displayed document and its exact write decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MigrationDocument {
    pub(crate) path: String,
    pub(crate) source_generation: Option<u32>,
    pub(crate) target_generation: u32,
    pub(crate) route: Vec<(u32, u32)>,
    pub(crate) will_publish: bool,
}

/// Data-only result after the confirmed inventory reaches its target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MigrationOutcome {
    pub(crate) target_generation: u32,
    pub(crate) published_documents: usize,
    pub(crate) normal_commands_available: bool,
}

/// The sole stable migration edge outside this module.
pub(crate) struct MigrationFacade;

impl MigrationFacade {
    fn plan_continuous(
        context: &ResolverContext,
        target: u32,
    ) -> Result<continuous::MigrationPlan, continuous::PlanError> {
        continuous::MigrationPlan::build(context, target)
    }

    pub(crate) fn present(
        request: &MigrationRequest,
    ) -> Result<MigrationPresentation, MigrationFacadeError> {
        let plan = Self::plan_continuous(&request.context, request.target_generation)
            .map_err(MigrationFacadeError)?;
        let summary = plan.presentation();
        Ok(MigrationPresentation {
            configuration_path: summary.configuration_path.display().to_string(),
            documents: summary
                .documents
                .into_iter()
                .map(|document| MigrationDocument {
                    path: document.path.display().to_string(),
                    source_generation: document.source_generation,
                    target_generation: document.target_generation,
                    route: document.route,
                    will_publish: document.will_publish,
                })
                .collect(),
            normal_commands_available: summary.normal_commands_available,
            ownership_invariant: "resource IDs, effects, and Known ownership do not change",
            broadened_ordering: summary.broadening,
            snapshot_fingerprint: plan.confirmation_fingerprint(),
        })
    }

    /// Rebuilds and compares the confirmed inventory before the first lock or publication.
    pub(crate) fn execute(
        request: MigrationRequest,
        presentation: &MigrationPresentation,
    ) -> Result<MigrationOutcome, MigrationFacadeError> {
        let mut plan = Self::plan_continuous(&request.context, request.target_generation)
            .map_err(MigrationFacadeError)?;
        if plan.confirmation_fingerprint() != presentation.snapshot_fingerprint {
            return Err(MigrationFacadeError(
                continuous::PlanError::ConfirmedSnapshotChanged,
            ));
        }
        let summary = plan.presentation();
        let published_documents = summary
            .documents
            .iter()
            .filter(|document| document.will_publish)
            .count();
        plan.execute_confirmed(&request.context)
            .map_err(MigrationFacadeError)?;
        Ok(MigrationOutcome {
            target_generation: request.target_generation,
            published_documents,
            normal_commands_available: summary.normal_commands_available,
        })
    }
}

/// A migration failure without route-private data or publication capability.
#[derive(Debug)]
pub(crate) struct MigrationFacadeError(continuous::PlanError);

impl fmt::Display for MigrationFacadeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for MigrationFacadeError {}

mod continuous;
mod schema_3_to_4;
mod schema_4_to_5;
mod schema_5_to_6;
