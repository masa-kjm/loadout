//! Pure Desired/Known/Actual classification and deterministic plan construction.

pub(crate) mod file_copy;
pub(crate) mod file_link;
pub(crate) mod ordering;

use crate::domain::actual::ActualState;
use crate::domain::desired::ResolvedDesired;
use crate::domain::diagnostic::Diagnostic;
use crate::domain::known::KnownState;
use crate::domain::plan::{Plan, PlannedResourceAction};
use crate::planner::ordering::sort_resource_actions;

/// One resource planner's private contribution to the aggregate plan.
pub(super) struct PlanContribution {
    resource_actions: Vec<PlannedResourceAction>,
    diagnostics: Vec<Diagnostic>,
}

impl PlanContribution {
    pub(super) fn new(
        resource_actions: impl IntoIterator<Item = PlannedResourceAction>,
        diagnostics: impl IntoIterator<Item = Diagnostic>,
    ) -> Self {
        Self {
            resource_actions: resource_actions.into_iter().collect(),
            diagnostics: diagnostics.into_iter().collect(),
        }
    }
}

/// Builds the one closed mixed-effect plan without performing filesystem access.
pub(crate) fn plan(desired: &ResolvedDesired, known: &KnownState, actual: &ActualState) -> Plan {
    let links = file_link::contribute(desired, known, actual);
    let copies = file_copy::contribute(desired, known, actual);
    let mut actions = links.resource_actions;
    actions.extend(copies.resource_actions);
    sort_resource_actions(&mut actions);
    Plan::new_with_resource_actions(
        actions,
        links.diagnostics.into_iter().chain(copies.diagnostics),
    )
    .expect("independent resource planners preserve globally unique desired targets")
}
