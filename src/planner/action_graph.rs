//! Pure compact action-graph construction and deterministic scheduling.
//!
//! Stage pair edges exist only in this function's local reachability set; the
//! resulting plan retains barrier units and memberships instead.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::desired::ResolvedDesired;
use crate::domain::diagnostic::Diagnostic;
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::known::{
    AppliedBarrier, AppliedDirectEdge, KnownState, StaleTopologySnapshot,
    TopologyBarrierReplacement,
};
use crate::domain::plan::{
    ActionKind, ActionRole, GraphAction, GraphBarrier, GraphDirectEdge, PlannedActionGraph,
    PlannedResourceAction,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct ActionGraph {
    graph: PlannedActionGraph,
    diagnostics: Vec<Diagnostic>,
}

impl ActionGraph {
    pub(crate) fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub(crate) fn plan_graph(&self) -> PlannedActionGraph {
        self.graph.clone()
    }

    pub(crate) fn into_actions(
        self,
        actions: Vec<PlannedResourceAction>,
    ) -> Vec<PlannedResourceAction> {
        if !self.diagnostics.is_empty() {
            return Vec::new();
        }
        let mut by_id = actions
            .into_iter()
            .map(|action| (action.resource_id().as_str().to_owned(), action))
            .collect::<BTreeMap<_, _>>();
        let mut ordered = self
            .graph
            .schedule
            .iter()
            .filter_map(|id| by_id.remove(id))
            .collect::<Vec<_>>();
        // Noops are reports, never graph nodes or executor work.
        ordered.extend(by_id.into_values());
        ordered
    }
}

/// Builds one combined DAG and a compact graph handoff without filesystem access.
pub(crate) fn schedule(
    desired: &ResolvedDesired,
    known: &KnownState,
    actions: &[PlannedResourceAction],
) -> ActionGraph {
    let mut result = ActionGraph::default();
    let roles = actions
        .iter()
        .filter_map(|action| {
            action
                .action_role()
                .map(|role| (action.resource_id().clone(), role))
        })
        .collect::<BTreeMap<_, _>>();
    let action_ids = roles.keys().cloned().collect::<BTreeSet<_>>();
    result.graph.actions = roles
        .iter()
        .map(|(id, role)| GraphAction {
            id: id.as_str().to_owned(),
            resource_id: id.clone(),
            role: *role,
            topology_authority: Default::default(),
            topology_delta: Default::default(),
        })
        .collect();

    let orchestration = desired.orchestration();
    let resources = orchestration
        .resources()
        .iter()
        .map(|fact| (fact.resource_id().clone(), fact))
        .collect::<BTreeMap<_, _>>();
    let mut direct_edges = BTreeSet::<(FullyQualifiedResourceId, FullyQualifiedResourceId)>::new();
    let mut satisfied_direct =
        BTreeSet::<(FullyQualifiedResourceId, FullyQualifiedResourceId)>::new();
    for (successor, fact) in &resources {
        for predecessor in fact.depends_on() {
            let Some(successor_role) = roles.get(successor).copied() else {
                continue;
            };
            match roles.get(predecessor).copied() {
                Some(predecessor_role) if legal_direct(predecessor_role, successor_role) => {
                    direct_edges.insert((predecessor.clone(), successor.clone()));
                    result.graph.direct_edges.push(GraphDirectEdge {
                        from: predecessor.as_str().to_owned(),
                        to: successor.as_str().to_owned(),
                        origin: "depends_on",
                    });
                }
                None if is_current(successor_role) && known.get_variant(predecessor).is_some() => {
                    satisfied_direct.insert((predecessor.clone(), successor.clone()));
                }
                _ => result.diagnostics.push(blocking(successor)),
            }
        }
    }

    let mut stage_members = BTreeMap::<String, Vec<FullyQualifiedResourceId>>::new();
    for (id, fact) in &resources {
        if let Some(stage) = fact.stage() {
            if action_ids.contains(id) {
                stage_members
                    .entry(stage.to_owned())
                    .or_default()
                    .push(id.clone());
            }
        }
    }
    for members in stage_members.values_mut() {
        members.sort();
    }
    let emitted_stage_ids = stage_members
        .iter()
        .filter_map(|(stage, members)| {
            orchestration
                .stages()
                .iter()
                .find(|candidate| candidate.id() == stage)
                .filter(|candidate| {
                    candidate
                        .after()
                        .iter()
                        .any(|after| stage_members.contains_key(after))
                })
                .map(|_| format!("stage/{stage}"))
                .filter(|_| !members.is_empty())
        })
        .collect::<BTreeSet<_>>();
    for stage in orchestration.stages() {
        let Some(outputs) = stage_members.get(stage.id()) else {
            continue;
        };
        let mut inputs = Vec::new();
        for after in stage.after() {
            let Some(members) = stage_members.get(after) else {
                continue;
            };
            inputs.extend(members.iter().cloned());
            for output in outputs {
                if members
                    .iter()
                    .any(|input| !legal_stage(roles[input], roles[output]))
                {
                    result.diagnostics.push(blocking(output));
                }
            }
        }
        inputs.sort();
        inputs.dedup();
        if !inputs.is_empty() {
            result.graph.barriers.push(GraphBarrier {
                id: format!("stage/{}", stage.id()),
                after: stage
                    .after()
                    .iter()
                    .map(|id| format!("stage/{id}"))
                    .filter(|id| emitted_stage_ids.contains(id))
                    .collect(),
                input: inputs.iter().map(|id| id.as_str().to_owned()).collect(),
                output: outputs.iter().map(|id| id.as_str().to_owned()).collect(),
                provenance: {
                    let mut provenance = std::iter::once(stage.id().to_owned())
                        .chain(stage.after().iter().cloned())
                        .collect::<Vec<_>>();
                    provenance.sort();
                    provenance.dedup();
                    provenance
                },
            });
        }
    }
    result
        .graph
        .direct_edges
        .sort_by(|l, r| (&l.from, &l.to, l.origin).cmp(&(&r.from, &r.to, r.origin)));
    result.graph.barriers.sort_by(|l, r| l.id.cmp(&r.id));
    let stale_snapshot = stale_snapshot(known, &roles);
    let stale_constraints = stale_constraints(&stale_snapshot, &roles, &mut result);
    result.graph.barriers.extend(stale_constraints);
    result.graph.barriers.sort_by(|l, r| l.id.cmp(&r.id));
    result.graph.stale_topology_snapshot = stale_snapshot;
    result.graph.schedule =
        topological_schedule(&action_ids, &direct_edges, &result.graph.barriers, &[]);
    if result.graph.schedule.len() != action_ids.len() {
        result.graph.schedule.clear();
        if let Some(id) = action_ids.first() {
            result.diagnostics.push(blocking(id));
        }
    }
    select_topology_publication(
        &mut result.graph,
        &satisfied_direct,
        orchestration,
        &resources,
    );
    result
}

/// Assigns each direct fact to the successor action that can verify and publish it.
fn select_topology_publication(
    graph: &mut PlannedActionGraph,
    satisfied_direct: &BTreeSet<(FullyQualifiedResourceId, FullyQualifiedResourceId)>,
    orchestration: &crate::domain::desired::ResolvedOrchestration,
    resources: &BTreeMap<
        FullyQualifiedResourceId,
        &crate::domain::desired::ResolvedResourceOrchestration,
    >,
) {
    let direct_facts = graph
        .direct_edges
        .iter()
        .map(|edge| {
            (
                FullyQualifiedResourceId::parse(&edge.from)
                    .expect("graph direct edges use canonical resource IDs"),
                FullyQualifiedResourceId::parse(&edge.to)
                    .expect("graph direct edges use canonical resource IDs"),
            )
        })
        .chain(satisfied_direct.iter().cloned())
        .collect::<BTreeSet<_>>();
    for (from, to) in direct_facts {
        let Some(node) = graph.actions.iter_mut().find(|node| node.resource_id == to) else {
            continue;
        };
        let fact = AppliedDirectEdge {
            from,
            to,
            origin: "depends_on",
        };
        node.topology_authority.current_direct.push(fact.clone());
        node.topology_delta.add_current_direct.push(fact);
    }
    let positions = graph
        .schedule
        .iter()
        .enumerate()
        .map(|(position, id)| (id.clone(), position))
        .collect::<BTreeMap<_, _>>();
    select_stage_topology_publication(graph, orchestration, resources, &positions);
    select_stale_topology_publication(graph, &positions);
}

/// Publishes stage facts only from actions that verified a member of that stage.
fn select_stage_topology_publication(
    graph: &mut PlannedActionGraph,
    orchestration: &crate::domain::desired::ResolvedOrchestration,
    resources: &BTreeMap<
        FullyQualifiedResourceId,
        &crate::domain::desired::ResolvedResourceOrchestration,
    >,
    positions: &BTreeMap<String, usize>,
) {
    let mut members = BTreeMap::<String, Vec<FullyQualifiedResourceId>>::new();
    let mut action_stages = BTreeMap::<String, Vec<String>>::new();
    for action in &graph.actions {
        if let Some(stage) = resources
            .get(&action.resource_id)
            .and_then(|resource| resource.stage())
        {
            members
                .entry(stage.to_owned())
                .or_default()
                .push(action.resource_id.clone());
            action_stages
                .entry(stage.to_owned())
                .or_default()
                .push(action.id.clone());
        }
    }
    for actions in action_stages.values_mut() {
        actions.sort_by_key(|id| positions.get(id).copied().unwrap_or(usize::MAX));
    }
    let stages = orchestration
        .stages()
        .iter()
        .map(|stage| (stage.id().to_owned(), stage))
        .collect::<BTreeMap<_, _>>();
    let facts = members
        .iter()
        .filter_map(|(stage_id, stage_members)| {
            stages.get(stage_id).map(|stage| {
                (
                    format!("stage/{stage_id}"),
                    AppliedBarrier {
                        id: format!("stage/{stage_id}"),
                        after: stage
                            .after()
                            .iter()
                            .map(|id| format!("stage/{id}"))
                            .collect(),
                        members: stage_members.clone(),
                        origin: "stage",
                    },
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    for stage_id in members.keys() {
        let Some(fact) = facts.get(&format!("stage/{stage_id}")) else {
            continue;
        };
        let owner = action_stages
            .get(stage_id)
            .and_then(|actions| actions.last())
            .cloned();
        let Some(owner) = owner else {
            continue;
        };
        if let Some(node) = graph.actions.iter_mut().find(|node| node.id == owner) {
            let mut pending = vec![fact.id.clone()];
            let mut authority = BTreeMap::new();
            while let Some(id) = pending.pop() {
                let Some(candidate) = facts.get(&id) else {
                    continue;
                };
                if authority.insert(id, candidate.clone()).is_none() {
                    pending.extend(candidate.after.iter().cloned());
                }
            }
            node.topology_authority
                .current_barriers
                .extend(authority.into_values());
            node.topology_delta.add_current_barriers.push(fact.clone());
        }
    }
}

fn final_scheduled_member(
    members: &[String],
    positions: &BTreeMap<String, usize>,
) -> Option<String> {
    members
        .iter()
        .filter_map(|member| positions.get(member).map(|position| (position, member)))
        .max_by(
            |(left_position, left_member), (right_position, right_member)| {
                left_position
                    .cmp(right_position)
                    .then_with(|| left_member.cmp(right_member))
            },
        )
        .map(|(_, member)| member.clone())
}

fn select_stale_topology_publication(
    graph: &mut PlannedActionGraph,
    positions: &BTreeMap<String, usize>,
) {
    let stale = graph
        .actions
        .iter()
        .filter(|action| {
            matches!(
                action.role,
                ActionRole::StaleRemove | ActionRole::ForgetMissing
            )
        })
        .map(|action| action.id.clone())
        .collect::<BTreeSet<_>>();
    for (is_current, edges) in [
        (true, &graph.stale_topology_snapshot.current_direct),
        (false, &graph.stale_topology_snapshot.historical_direct),
    ] {
        for edge in edges {
            // In a fully stale relation the successor owns the reversed removal.
            // An incident mixed fact has exactly one stale endpoint, which is its sole owner.
            let owner = if stale.contains(edge.to.as_str()) {
                edge.to.as_str()
            } else {
                edge.from.as_str()
            };
            if !stale.contains(owner) {
                continue;
            }
            if let Some(node) = graph.actions.iter_mut().find(|node| node.id == owner) {
                if is_current {
                    node.topology_authority.current_direct.push(edge.clone());
                    node.topology_delta.remove_current_direct.push(edge.clone());
                } else {
                    node.topology_authority.historical_direct.push(edge.clone());
                    node.topology_delta
                        .remove_historical_direct
                        .push(edge.clone());
                }
            }
        }
    }
    for (is_current, barriers) in [
        (true, &graph.stale_topology_snapshot.current_barriers),
        (false, &graph.stale_topology_snapshot.historical_barriers),
    ] {
        for barrier in barriers {
            let mut owners = barrier
                .members
                .iter()
                .filter(|member| stale.contains(member.as_str()))
                .map(|member| member.as_str())
                .collect::<Vec<_>>();
            owners.sort_by_key(|owner| positions.get(*owner).copied().unwrap_or(usize::MAX));
            let mut before = barrier.clone();
            for owner in owners {
                let mut after = before.clone();
                after.members.retain(|member| member.as_str() != owner);
                let Some(node) = graph.actions.iter_mut().find(|node| node.id == owner) else {
                    continue;
                };
                if is_current {
                    node.topology_authority
                        .current_barriers
                        .push(before.clone());
                    if after.members.is_empty() {
                        node.topology_delta
                            .remove_current_barriers
                            .push(before.clone());
                    } else {
                        node.topology_delta.replace_current_barriers.push(
                            TopologyBarrierReplacement {
                                before: before.clone(),
                                after: after.clone(),
                            },
                        );
                    }
                } else {
                    node.topology_authority
                        .historical_barriers
                        .push(before.clone());
                    if after.members.is_empty() {
                        node.topology_delta
                            .remove_historical_barriers
                            .push(before.clone());
                    } else {
                        node.topology_delta.replace_historical_barriers.push(
                            TopologyBarrierReplacement {
                                before: before.clone(),
                                after: after.clone(),
                            },
                        );
                    }
                }
                before = after;
            }
        }
    }
}

fn is_current(role: ActionRole) -> bool {
    matches!(
        role,
        ActionRole::CurrentCreate | ActionRole::CurrentChange | ActionRole::CurrentHandoff
    )
}
/// The table is deliberately closed: no generic pair fallback is permitted.
fn legal_direct(predecessor: ActionRole, successor: ActionRole) -> bool {
    is_current(predecessor) && is_current(successor)
}
fn legal_stage(predecessor: ActionRole, successor: ActionRole) -> bool {
    is_current(predecessor) && is_current(successor)
}

fn topological_schedule(
    action_ids: &BTreeSet<FullyQualifiedResourceId>,
    direct_edges: &BTreeSet<(FullyQualifiedResourceId, FullyQualifiedResourceId)>,
    barriers: &[GraphBarrier],
    stale_barriers: &[GraphBarrier],
) -> Vec<String> {
    let mut incoming = action_ids
        .iter()
        .cloned()
        .map(|id| (id, 0usize))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing = BTreeMap::<String, Vec<String>>::new();
    let mut barrier_remaining = BTreeMap::<String, usize>::new();
    let mut barrier_outputs = BTreeMap::<String, Vec<String>>::new();
    for (from, to) in direct_edges {
        *incoming.get_mut(to).expect("graph edges name actions") += 1;
        outgoing
            .entry(from.as_str().to_owned())
            .or_default()
            .push(to.as_str().to_owned());
    }
    for barrier in barriers.iter().chain(stale_barriers) {
        barrier_remaining.insert(barrier.id.clone(), barrier.input.len());
        barrier_outputs.insert(barrier.id.clone(), barrier.output.clone());
        for input in &barrier.input {
            outgoing
                .entry(input.clone())
                .or_default()
                .push(barrier.id.clone());
        }
        for output in &barrier.output {
            *incoming
                .get_mut(&FullyQualifiedResourceId::parse(output).expect("canonical action ID"))
                .expect("barriers name actions") += 1;
        }
    }
    let mut ready = incoming
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let mut schedule = Vec::new();
    while let Some(id) = ready.pop_first() {
        schedule.push(id.as_str().to_owned());
        for successor in outgoing.get(id.as_str()).into_iter().flatten() {
            if let Some(remaining) = barrier_remaining.get_mut(successor) {
                *remaining -= 1;
                if *remaining == 0 {
                    for output in &barrier_outputs[successor] {
                        let output =
                            FullyQualifiedResourceId::parse(output).expect("canonical action ID");
                        let count = incoming.get_mut(&output).expect("barriers name actions");
                        *count -= 1;
                        if *count == 0 {
                            ready.insert(output);
                        }
                    }
                }
            } else {
                let successor =
                    FullyQualifiedResourceId::parse(successor).expect("graph edges name actions");
                let count = incoming
                    .get_mut(&successor)
                    .expect("graph edges name actions");
                *count -= 1;
                if *count == 0 {
                    ready.insert(successor);
                }
            }
        }
    }
    schedule
}

fn stale_snapshot(
    known: &KnownState,
    roles: &BTreeMap<FullyQualifiedResourceId, ActionRole>,
) -> StaleTopologySnapshot {
    let stale = |id: &FullyQualifiedResourceId| {
        matches!(
            roles.get(id),
            Some(ActionRole::StaleRemove | ActionRole::ForgetMissing)
        )
    };
    let select_direct = |facts: &[AppliedDirectEdge]| {
        facts
            .iter()
            .filter(|fact| stale(&fact.from) || stale(&fact.to))
            .cloned()
            .collect()
    };
    let select_barriers = |facts: &[AppliedBarrier]| {
        let mut selected = facts
            .iter()
            .filter(|fact| fact.members.iter().any(stale))
            .cloned()
            .collect::<Vec<_>>();
        loop {
            let additions = selected
                .iter()
                .flat_map(|barrier| barrier.after.iter())
                .filter_map(|id| facts.iter().find(|candidate| candidate.id == *id))
                .filter(|candidate| !selected.iter().any(|existing| existing.id == candidate.id))
                .cloned()
                .collect::<Vec<_>>();
            if additions.is_empty() {
                break;
            }
            selected.extend(additions);
        }
        selected.sort_by(|left, right| left.id.cmp(&right.id));
        selected
    };
    StaleTopologySnapshot {
        current_direct: select_direct(known.topology().current_direct()),
        historical_direct: select_direct(known.topology().historical_direct()),
        current_barriers: select_barriers(known.topology().current_barriers()),
        historical_barriers: select_barriers(known.topology().historical_barriers()),
    }
}

fn stale_constraints(
    snapshot: &StaleTopologySnapshot,
    roles: &BTreeMap<FullyQualifiedResourceId, ActionRole>,
    result: &mut ActionGraph,
) -> Vec<GraphBarrier> {
    let stale = |id: &FullyQualifiedResourceId| {
        matches!(
            roles.get(id),
            Some(ActionRole::StaleRemove | ActionRole::ForgetMissing)
        )
    };
    let mut constraints = Vec::new();
    for edge in snapshot
        .current_direct
        .iter()
        .chain(&snapshot.historical_direct)
    {
        if stale(&edge.from) && stale(&edge.to) {
            constraints.push(GraphBarrier {
                id: format!("stale/direct/{}/{}", edge.from, edge.to),
                after: vec![],
                input: vec![edge.to.as_str().to_owned()],
                output: vec![edge.from.as_str().to_owned()],
                provenance: vec![edge.from.as_str().to_owned(), edge.to.as_str().to_owned()],
            });
        }
    }
    for barrier in snapshot
        .current_barriers
        .iter()
        .chain(&snapshot.historical_barriers)
    {
        if !barrier.members.iter().all(stale) {
            continue;
        }
        for after in &barrier.after {
            let Some(predecessor) = snapshot
                .current_barriers
                .iter()
                .chain(&snapshot.historical_barriers)
                .find(|candidate| candidate.id == *after)
            else {
                continue;
            };
            if !predecessor.members.iter().all(stale) {
                continue;
            }
            constraints.push(GraphBarrier {
                id: stale_barrier_constraint_id(&barrier.id, after),
                after: vec![],
                input: barrier
                    .members
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect(),
                output: predecessor
                    .members
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect(),
                provenance: vec![barrier.id.clone(), after.clone()],
            });
        }
    }
    for constraint in &constraints {
        if constraint
            .input
            .iter()
            .any(|input| constraint.output.contains(input))
        {
            if let Some(id) = constraint
                .input
                .first()
                .and_then(|id| FullyQualifiedResourceId::parse(id).ok())
            {
                result.diagnostics.push(blocking(&id));
            }
        }
    }
    constraints
}

/// Returns an injective synthetic node ID for one reversed compact barrier relation.
///
/// The length-delimited canonical IDs avoid collisions when a qualified stage ID
/// itself contains the separator used by this internal node namespace.
fn stale_barrier_constraint_id(barrier_id: &str, after: &str) -> String {
    format!(
        "stale/barrier/{}:{barrier_id}/after/{}:{after}",
        barrier_id.len(),
        after.len()
    )
}

// W3 cannot widen CLI diagnostics, which W4 owns. This existing blocking envelope
// preserves fail-closed behavior until schema-5 diagnostics are serialized there.
fn blocking(resource_id: &FullyQualifiedResourceId) -> Diagnostic {
    Diagnostic::UnsupportedPlatform {
        resource_id: resource_id.clone(),
        action_kind: ActionKind::Noop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::desired::{
        ResolvedOrchestration, ResolvedResourceOrchestration, ResolvedStage,
    };
    use crate::domain::file_link::ResolvedFileLink;
    use crate::domain::ids::ProfileId;
    use crate::domain::paths::ResolvedPath;
    use crate::domain::plan::PlannedAction;
    fn resource(id: &str) -> ResolvedFileLink {
        let root = std::env::temp_dir().join("loadout-action-graph");
        ResolvedFileLink::new(
            FullyQualifiedResourceId::parse(id).unwrap(),
            ResolvedPath::new(root.join("store").join(id.replace('/', "-"))).unwrap(),
            ResolvedPath::new(root.join("home").join(id.replace('/', "-"))).unwrap(),
        )
        .unwrap()
    }
    fn desired(
        resources: Vec<ResolvedFileLink>,
        orchestration: ResolvedOrchestration,
    ) -> ResolvedDesired {
        ResolvedDesired::new(ProfileId::parse("root").unwrap(), resources)
            .unwrap()
            .with_orchestration(orchestration)
    }
    #[test]
    fn direct_and_stage_edges_form_one_cycle_checked_before_scheduling() {
        let alpha = resource("root/alpha");
        let beta = resource("root/beta");
        let orchestration = ResolvedOrchestration::new(
            vec![
                ResolvedStage::new("root/first".into(), vec!["root/second".into()]),
                ResolvedStage::new("root/second".into(), vec![]),
            ],
            vec![
                ResolvedResourceOrchestration::new(
                    alpha.resource_id().clone(),
                    Some("root/first".into()),
                    vec![],
                ),
                ResolvedResourceOrchestration::new(
                    beta.resource_id().clone(),
                    Some("root/second".into()),
                    vec![alpha.resource_id().clone()],
                ),
            ],
        );
        let desired = desired(vec![alpha.clone(), beta.clone()], orchestration);
        let actions = vec![
            PlannedAction::create_link(alpha).into(),
            PlannedAction::create_link(beta).into(),
        ];
        let graph = schedule(&desired, &KnownState::empty(), &actions);
        assert!(!graph.diagnostics().is_empty());
        assert!(graph.into_actions(actions).is_empty());
    }
    #[test]
    fn barrier_is_compact_and_schedule_uses_resource_id_tie_breaking() {
        let alpha = resource("root/alpha");
        let beta = resource("root/beta");
        let gamma = resource("root/gamma");
        let orchestration = ResolvedOrchestration::new(
            vec![
                ResolvedStage::new("root/first".into(), vec![]),
                ResolvedStage::new("root/second".into(), vec!["root/first".into()]),
            ],
            vec![
                ResolvedResourceOrchestration::new(
                    alpha.resource_id().clone(),
                    Some("root/first".into()),
                    vec![],
                ),
                ResolvedResourceOrchestration::new(
                    beta.resource_id().clone(),
                    Some("root/first".into()),
                    vec![],
                ),
                ResolvedResourceOrchestration::new(
                    gamma.resource_id().clone(),
                    Some("root/second".into()),
                    vec![],
                ),
            ],
        );
        let desired = desired(
            vec![gamma.clone(), beta.clone(), alpha.clone()],
            orchestration,
        );
        let actions = vec![
            PlannedAction::create_link(gamma).into(),
            PlannedAction::create_link(beta).into(),
            PlannedAction::create_link(alpha).into(),
        ];
        let compact = schedule(&desired, &KnownState::empty(), &actions).plan_graph();
        assert_eq!(compact.barriers.len(), 1);
        assert_eq!(compact.barriers[0].input, ["root/alpha", "root/beta"]);
        assert_eq!(compact.barriers[0].output, ["root/gamma"]);
        assert!(compact.direct_edges.is_empty());
        assert_eq!(compact.schedule, ["root/alpha", "root/beta", "root/gamma"]);
    }

    #[test]
    fn stage_barrier_provenance_is_canonicalized_before_graph_persistence() {
        let early = resource("root/early");
        let late = resource("root/late");
        let desired = desired(
            vec![early.clone(), late.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("root/a".into(), vec![]),
                    ResolvedStage::new("root/z".into(), vec!["root/a".into()]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        early.resource_id().clone(),
                        Some("root/a".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        late.resource_id().clone(),
                        Some("root/z".into()),
                        vec![],
                    ),
                ],
            ),
        );

        let graph = schedule(
            &desired,
            &KnownState::empty(),
            &[
                PlannedAction::create_link(early).into(),
                PlannedAction::create_link(late).into(),
            ],
        )
        .plan_graph();

        assert_eq!(graph.barriers[0].provenance, ["root/a", "root/z"]);
    }

    #[test]
    fn stale_constraints_are_sorted_with_current_stage_barriers() {
        let stale_a = resource("root/stale-a");
        let stale_b = resource("root/stale-b");
        let early = resource("root/early");
        let late = resource("root/late");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&stale_a),
            crate::domain::known::KnownFileLink::from_resolved(&stale_b),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![AppliedDirectEdge {
                from: stale_a.resource_id().clone(),
                to: stale_b.resource_id().clone(),
                origin: "depends_on",
            }],
            vec![],
            vec![],
            vec![],
        ));
        let desired = desired(
            vec![early.clone(), late.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("root/a".into(), vec![]),
                    ResolvedStage::new("root/z".into(), vec!["root/a".into()]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        early.resource_id().clone(),
                        Some("root/a".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        late.resource_id().clone(),
                        Some("root/z".into()),
                        vec![],
                    ),
                ],
            ),
        );

        let graph = schedule(
            &desired,
            &known,
            &[
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(
                    &stale_a,
                ))
                .into(),
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(
                    &stale_b,
                ))
                .into(),
                PlannedAction::create_link(early).into(),
                PlannedAction::create_link(late).into(),
            ],
        )
        .plan_graph();

        assert!(
            graph
                .barriers
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id)
        );
        assert!(
            graph
                .barriers
                .iter()
                .any(|barrier| barrier.id.starts_with("stale/"))
        );
        assert!(
            graph
                .barriers
                .iter()
                .any(|barrier| barrier.id == "stage/root/z")
        );
    }

    #[test]
    fn forward_barrier_is_published_only_by_its_final_output_after_all_inputs() {
        let input_a = resource("root/input-a");
        let input_b = resource("root/input-b");
        let output_a = resource("root/output-a");
        let output_b = resource("root/output-b");
        let desired = desired(
            vec![
                input_a.clone(),
                input_b.clone(),
                output_a.clone(),
                output_b.clone(),
            ],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("root/input".into(), vec![]),
                    ResolvedStage::new("root/output".into(), vec!["root/input".into()]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        input_a.resource_id().clone(),
                        Some("root/input".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        input_b.resource_id().clone(),
                        Some("root/input".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        output_a.resource_id().clone(),
                        Some("root/output".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        output_b.resource_id().clone(),
                        Some("root/output".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let graph = schedule(
            &desired,
            &KnownState::empty(),
            &[
                PlannedAction::create_link(input_a).into(),
                PlannedAction::create_link(input_b).into(),
                PlannedAction::create_link(output_a).into(),
                PlannedAction::create_link(output_b).into(),
            ],
        )
        .plan_graph();

        assert_eq!(
            graph.schedule,
            [
                "root/input-a",
                "root/input-b",
                "root/output-a",
                "root/output-b"
            ]
        );
        let publishers = graph
            .actions
            .iter()
            .filter(|action| !action.topology_delta.add_current_barriers.is_empty())
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(publishers, ["root/input-b", "root/output-b"]);
    }

    #[test]
    fn direct_successor_carries_its_selected_topology_authority_and_delta() {
        let alpha = resource("root/alpha");
        let beta = resource("root/beta");
        let orchestration = ResolvedOrchestration::new(
            vec![],
            vec![
                ResolvedResourceOrchestration::new(alpha.resource_id().clone(), None, vec![]),
                ResolvedResourceOrchestration::new(
                    beta.resource_id().clone(),
                    None,
                    vec![alpha.resource_id().clone()],
                ),
            ],
        );
        let desired = desired(vec![alpha.clone(), beta.clone()], orchestration);
        let actions = vec![
            PlannedAction::create_link(beta).into(),
            PlannedAction::create_link(alpha).into(),
        ];

        let compact = schedule(&desired, &KnownState::empty(), &actions).plan_graph();
        let beta = compact
            .actions
            .iter()
            .find(|node| node.id == "root/beta")
            .unwrap();
        assert_eq!(beta.topology_authority.current_direct.len(), 1);
        assert_eq!(beta.topology_delta.add_current_direct.len(), 1);
    }

    #[test]
    fn stale_stage_relation_is_rejected_by_closed_legality_table() {
        let stale = resource("root/stale");
        let current = resource("root/current");
        let known =
            KnownState::new([crate::domain::known::KnownFileLink::from_resolved(&stale)]).unwrap();
        let orchestration = ResolvedOrchestration::new(
            vec![
                ResolvedStage::new("root/old".into(), vec![]),
                ResolvedStage::new("root/new".into(), vec!["root/old".into()]),
            ],
            vec![
                ResolvedResourceOrchestration::new(
                    stale.resource_id().clone(),
                    Some("root/old".into()),
                    vec![],
                ),
                ResolvedResourceOrchestration::new(
                    current.resource_id().clone(),
                    Some("root/new".into()),
                    vec![],
                ),
            ],
        );
        let desired = desired(vec![current.clone()], orchestration);
        let actions = vec![
            PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(&stale))
                .into(),
            PlannedAction::create_link(current).into(),
        ];
        assert!(
            !schedule(&desired, &known, &actions)
                .diagnostics()
                .is_empty()
        );
    }

    #[test]
    fn stale_direct_topology_reverses_only_committed_stale_relation_and_selects_successor_owner() {
        let alpha = resource("root/alpha");
        let beta = resource("root/beta");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&alpha),
            crate::domain::known::KnownFileLink::from_resolved(&beta),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![AppliedDirectEdge {
                from: alpha.resource_id().clone(),
                to: beta.resource_id().clone(),
                origin: "depends_on",
            }],
            vec![],
            vec![],
            vec![],
        ));
        let actions = vec![
            PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(&alpha))
                .into(),
            PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(&beta))
                .into(),
        ];
        let graph = schedule(
            &desired(vec![], ResolvedOrchestration::default()),
            &known,
            &actions,
        )
        .plan_graph();
        assert_eq!(graph.schedule, ["root/beta", "root/alpha"]);
        let beta = graph
            .actions
            .iter()
            .find(|action| action.id == "root/beta")
            .unwrap();
        assert_eq!(beta.topology_delta.remove_current_direct.len(), 1);
        assert_eq!(graph.stale_topology_snapshot.current_direct.len(), 1);
    }

    #[test]
    fn stale_mixed_barrier_projects_only_the_stale_member_without_a_scheduling_edge() {
        let stale = resource("root/stale");
        let current = resource("root/current");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&stale),
            crate::domain::known::KnownFileLink::from_resolved(&current),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![],
            vec![],
            vec![AppliedBarrier {
                id: "stage/root/late".into(),
                after: vec!["stage/root/early".into()],
                members: vec![current.resource_id().clone(), stale.resource_id().clone()],
                origin: "stage",
            }],
            vec![],
        ));
        let actions = vec![
            PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(&stale))
                .into(),
        ];
        let graph = schedule(
            &desired(vec![], ResolvedOrchestration::default()),
            &known,
            &actions,
        )
        .plan_graph();
        let action = &graph.actions[0];
        assert_eq!(action.topology_delta.replace_current_barriers.len(), 1);
        assert_eq!(
            action.topology_delta.replace_current_barriers[0]
                .after
                .members,
            [current.resource_id().clone()]
        );
        assert_eq!(graph.schedule, ["root/stale"]);
    }

    #[test]
    fn stale_endpoint_owns_an_incident_current_direct_fact_without_a_mixed_schedule_edge() {
        let stale = resource("root/stale");
        let current = resource("root/current");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&stale),
            crate::domain::known::KnownFileLink::from_resolved(&current),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![AppliedDirectEdge {
                from: stale.resource_id().clone(),
                to: current.resource_id().clone(),
                origin: "depends_on",
            }],
            vec![],
            vec![],
            vec![],
        ));
        let graph = schedule(
            &desired(vec![], ResolvedOrchestration::default()),
            &known,
            &[
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(
                    &stale,
                ))
                .into(),
            ],
        )
        .plan_graph();
        assert_eq!(graph.schedule, ["root/stale"]);
        assert_eq!(
            graph.actions[0].topology_delta.remove_current_direct.len(),
            1
        );
    }

    #[test]
    fn stale_multi_after_barrier_uses_distinct_reverse_constraint_nodes() {
        let early_a = resource("root/early-a");
        let early_b = resource("root/early-b");
        let late = resource("root/late");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&early_a),
            crate::domain::known::KnownFileLink::from_resolved(&early_b),
            crate::domain::known::KnownFileLink::from_resolved(&late),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![],
            vec![],
            vec![
                AppliedBarrier {
                    id: "stage/root/early-a".into(),
                    after: vec![],
                    members: vec![early_a.resource_id().clone()],
                    origin: "stage",
                },
                AppliedBarrier {
                    id: "stage/root/early-b".into(),
                    after: vec![],
                    members: vec![early_b.resource_id().clone()],
                    origin: "stage",
                },
                AppliedBarrier {
                    id: "stage/root/late".into(),
                    after: vec!["stage/root/early-a".into(), "stage/root/early-b".into()],
                    members: vec![late.resource_id().clone()],
                    origin: "stage",
                },
            ],
            vec![],
        ));
        let graph = schedule(
            &desired(vec![], ResolvedOrchestration::default()),
            &known,
            &[
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(
                    &early_a,
                ))
                .into(),
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(
                    &early_b,
                ))
                .into(),
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(
                    &late,
                ))
                .into(),
            ],
        )
        .plan_graph();

        assert_eq!(
            graph.schedule,
            ["root/late", "root/early-a", "root/early-b"]
        );
        let late = graph
            .actions
            .iter()
            .find(|action| action.id == "root/late")
            .unwrap();
        assert_eq!(late.topology_delta.remove_current_barriers.len(), 1);
        let early_a = graph
            .actions
            .iter()
            .find(|action| action.id == "root/early-a")
            .unwrap();
        assert_eq!(early_a.topology_delta.remove_current_barriers.len(), 1);

        let mut topology = known.topology().clone();
        for action_id in &graph.schedule {
            let action = graph
                .actions
                .iter()
                .find(|action| &action.id == action_id)
                .unwrap();
            topology = topology
                .transition(&action.topology_authority, &action.topology_delta)
                .unwrap();
        }
        assert_eq!(
            topology,
            crate::domain::known::AppliedLifecycleTopology::default()
        );
    }

    #[test]
    fn large_barrier_uses_membership_nodes_not_a_pair_edge_cross_product() {
        let inputs = (0..32)
            .map(|index| resource(&format!("root/in-{index:02}")))
            .collect::<Vec<_>>();
        let outputs = (0..32)
            .map(|index| resource(&format!("root/out-{index:02}")))
            .collect::<Vec<_>>();
        let mut orchestration_resources = Vec::new();
        for resource in &inputs {
            orchestration_resources.push(ResolvedResourceOrchestration::new(
                resource.resource_id().clone(),
                Some("root/first".into()),
                vec![],
            ));
        }
        for resource in &outputs {
            orchestration_resources.push(ResolvedResourceOrchestration::new(
                resource.resource_id().clone(),
                Some("root/second".into()),
                vec![],
            ));
        }
        let desired = desired(
            inputs.iter().chain(&outputs).cloned().collect(),
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("root/first".into(), vec![]),
                    ResolvedStage::new("root/second".into(), vec!["root/first".into()]),
                ],
                orchestration_resources,
            ),
        );
        let actions = inputs
            .iter()
            .chain(&outputs)
            .cloned()
            .map(|resource| PlannedAction::create_link(resource).into())
            .collect::<Vec<_>>();
        let graph = schedule(&desired, &KnownState::empty(), &actions).plan_graph();
        assert_eq!(graph.direct_edges.len(), 0);
        assert_eq!(graph.barriers.len(), 1);
        assert_eq!(
            graph.barriers[0].input.len() + graph.barriers[0].output.len(),
            64
        );
        assert_eq!(graph.schedule.len(), 64);
    }
}
