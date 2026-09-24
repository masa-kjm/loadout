//! Pure Desired/Known/Actual classification and deterministic plan construction.

pub(crate) mod file_copy;
pub(crate) mod file_link;
pub(crate) mod ordering;

use crate::domain::actual::ActualState;
use crate::domain::desired::ResolvedDesired;
use crate::domain::known::KnownState;
use crate::domain::plan::Plan;

/// Builds the one closed mixed-effect plan without performing filesystem access.
pub(crate) fn plan(desired: &ResolvedDesired, known: &KnownState, actual: &ActualState) -> Plan {
    let links = file_link::plan(desired, known, actual);
    let copies = file_copy::plan(desired, known, actual);
    Plan::new_with_resource_actions(
        links
            .resource_actions()
            .iter()
            .chain(copies.resource_actions())
            .cloned(),
        links
            .diagnostics()
            .iter()
            .chain(copies.diagnostics())
            .cloned(),
    )
    .expect("independent resource planners preserve globally unique desired targets")
}
