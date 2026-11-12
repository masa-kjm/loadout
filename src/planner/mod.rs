//! Pure Desired/Known/Actual classification and deterministic plan construction.

pub(crate) mod action_graph;
pub(crate) mod directory_link;
pub(crate) mod file_copy;
pub(crate) mod file_link;
pub(crate) mod ordering;

use crate::domain::actual::ActualState;
use crate::domain::desired::ResolvedDesired;
use crate::domain::diagnostic::Diagnostic;
use crate::domain::directory_link::ActualDirectoryLinkState;
use crate::domain::known::KnownState;
use crate::domain::plan::{Plan, PlannedResourceAction};
use crate::planner::action_graph::schedule;

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
    let graph = schedule(desired, known, &actions);
    let graph_diagnostics = graph.diagnostics().to_vec();
    let graph_plan = graph.plan_graph();
    actions = graph.into_actions(actions);
    Plan::new_with_graph(
        actions,
        links
            .diagnostics
            .into_iter()
            .chain(copies.diagnostics)
            .chain(graph_diagnostics),
        graph_plan,
    )
    .expect("independent resource planners preserve globally unique desired targets")
}

/// Builds a plan with DirectoryLink observations supplied by the dedicated no-follow inspector.
pub(crate) fn plan_with_directory_links(
    desired: &ResolvedDesired,
    known: &KnownState,
    actual: &ActualState,
    directory_actual: &ActualDirectoryLinkState,
) -> Plan {
    let links = file_link::contribute(desired, known, actual);
    let copies = file_copy::contribute(desired, known, actual);
    let directories = directory_link::contribute(desired, known, directory_actual);
    let mut actions = links.resource_actions;
    actions.extend(copies.resource_actions);
    actions.extend(directories.resource_actions);
    let graph = schedule(desired, known, &actions);
    let graph_diagnostics = graph.diagnostics().to_vec();
    let graph_plan = graph.plan_graph();
    actions = graph.into_actions(actions);
    Plan::new_with_graph(
        actions,
        links
            .diagnostics
            .into_iter()
            .chain(copies.diagnostics)
            .chain(directories.diagnostics)
            .chain(graph_diagnostics),
        graph_plan,
    )
    .expect("independent resource planners preserve globally unique desired targets")
}
