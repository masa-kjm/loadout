//! Pure compact action-graph construction and deterministic scheduling.
//!
//! Stage pair edges exist only in this function's local reachability set; the
//! resulting plan retains barrier units and memberships instead.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::desired::ResolvedDesired;
use crate::domain::diagnostic::{Diagnostic, OrderingBlockReason};
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::known::{
    AppliedBarrier, AppliedDirectEdge, KnownState, StaleTopologySnapshot, TopologyBarrierAdvance,
    TopologyBarrierReplacement,
};
use crate::domain::plan::{
    ActionRole, GraphAction, GraphBarrier, GraphDirectEdge, PlannedActionGraph,
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
    result
        .diagnostics
        .extend(recorded_order_conflicts(desired, known));
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
                Some(_) => result.diagnostics.push(blocking(
                    successor,
                    OrderingBlockReason::UnsupportedActionRoles,
                )),
                None => result.diagnostics.push(blocking(
                    successor,
                    OrderingBlockReason::UnprovedPredecessor,
                )),
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
    for stage in orchestration.stages() {
        let mut inputs = Vec::new();
        for after in stage.after() {
            if let Some(members) = stage_members.get(after) {
                inputs.extend(members.iter().cloned());
                if let Some(outputs) = stage_members.get(stage.id()) {
                    for output in outputs {
                        if members
                            .iter()
                            .any(|input| !legal_stage(roles[input], roles[output]))
                        {
                            result.diagnostics.push(blocking(
                                output,
                                OrderingBlockReason::UnsupportedActionRoles,
                            ));
                        }
                    }
                }
            }
        }
        inputs.sort();
        inputs.dedup();
        result.graph.barriers.push(GraphBarrier {
            id: stage.id().to_owned(),
            after: stage.after().to_vec(),
            input: inputs.iter().map(|id| id.as_str().to_owned()).collect(),
            output: stage_members
                .get(stage.id())
                .into_iter()
                .flatten()
                .map(|id| id.as_str().to_owned())
                .collect(),
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
    result.diagnostics.extend(attach_recorded_order_constraints(
        &mut result.graph,
        known,
        &resources,
        &roles,
    ));
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
            result
                .diagnostics
                .push(blocking(id, OrderingBlockReason::Cycle));
        }
    }
    let publication_blocks = select_topology_publication(
        &mut result.graph,
        &satisfied_direct,
        orchestration,
        &resources,
        known,
    );
    result.diagnostics.extend(
        publication_blocks
            .iter()
            .map(|id| blocking(id, OrderingBlockReason::UnprovedTopologyPublication)),
    );
    result
}

/// Retains old barrier order when a new member joins a previously recorded phase.
fn attach_recorded_order_constraints(
    graph: &mut PlannedActionGraph,
    known: &KnownState,
    resources: &BTreeMap<
        FullyQualifiedResourceId,
        &crate::domain::desired::ResolvedResourceOrchestration,
    >,
    roles: &BTreeMap<FullyQualifiedResourceId, ActionRole>,
) -> Vec<Diagnostic> {
    let recorded = known
        .topology()
        .current_barriers()
        .iter()
        .chain(known.topology().historical_barriers())
        .map(|fact| (fact.id.as_str(), fact))
        .collect::<BTreeMap<_, _>>();
    let mut blocked = Vec::new();
    for action in &graph.actions {
        if !is_current(action.role) {
            continue;
        }
        let Some(stage) = resources
            .get(&action.resource_id)
            .and_then(|resource| resource.stage())
        else {
            continue;
        };
        let Some(existing) = recorded.get(stage) else {
            continue;
        };
        if existing.members.contains(&action.resource_id) {
            continue;
        }
        let Some(output) = graph
            .barriers
            .iter_mut()
            .find(|barrier| barrier.id == stage)
        else {
            blocked.push(blocking(
                &action.resource_id,
                OrderingBlockReason::UnprovedRecordedPredecessor,
            ));
            continue;
        };
        let mut pending = existing.after.clone();
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let Some(predecessor) = recorded.get(id.as_str()) else {
                blocked.push(blocking(
                    &action.resource_id,
                    OrderingBlockReason::UnprovedRecordedPredecessor,
                ));
                break;
            };
            output.provenance.push(id);
            pending.extend(predecessor.after.iter().cloned());
            for member in &predecessor.members {
                match roles.get(member) {
                    Some(role) if is_current(*role) => {
                        output.input.push(member.as_str().to_owned());
                    }
                    None if known.get_variant(member).is_some()
                        && resources.contains_key(member) => {}
                    _ => blocked.push(blocking(
                        &action.resource_id,
                        OrderingBlockReason::UnprovedRecordedPredecessor,
                    )),
                }
            }
        }
        output.input.sort();
        output.input.dedup();
        output.provenance.sort();
        output.provenance.dedup();
        if output.input.iter().any(|id| output.output.contains(id)) {
            blocked.push(blocking(
                &action.resource_id,
                OrderingBlockReason::UnprovedRecordedPredecessor,
            ));
        }
    }
    blocked
}

type OrderingEdges = BTreeMap<String, BTreeMap<String, String>>;

/// Compares resource reachability, including ordering that has no current action.
fn recorded_order_conflicts(desired: &ResolvedDesired, known: &KnownState) -> Vec<Diagnostic> {
    let mut recorded = OrderingEdges::new();
    let topology = known.topology();
    for (array, facts) in [
        ("current", topology.current_direct()),
        ("historical", topology.historical_direct()),
    ] {
        for fact in facts {
            add_ordering_edge(
                &mut recorded,
                fact.from.as_str(),
                fact.to.as_str(),
                format!("recorded {array} direct {} -> {}", fact.from, fact.to),
            );
        }
    }
    let barriers = topology
        .current_barriers()
        .iter()
        .map(|barrier| (barrier.id.as_str(), ("current", barrier)))
        .chain(
            topology
                .historical_barriers()
                .iter()
                .map(|barrier| (barrier.id.as_str(), ("historical", barrier))),
        )
        .collect::<BTreeMap<_, _>>();
    for (array, barrier) in barriers.values() {
        for predecessor in &barrier.after {
            let Some((input_array, input)) = barriers.get(predecessor.as_str()) else {
                continue;
            };
            for from in &input.members {
                for to in &barrier.members {
                    add_ordering_edge(
                        &mut recorded,
                        from.as_str(),
                        to.as_str(),
                        format!(
                            "recorded {input_array} phase {} -> {array} phase {}",
                            input.id, barrier.id
                        ),
                    );
                }
            }
        }
    }

    let orchestration = desired.orchestration();
    let mut proposed = OrderingEdges::new();
    let mut phase_members = BTreeMap::<&str, Vec<(&str, Option<&str>)>>::new();
    for resource in orchestration.resources() {
        for predecessor in resource.depends_on() {
            let source = resource
                .dependency_sources()
                .iter()
                .find(|(id, _)| id == predecessor)
                .map(|(_, source)| source.clone())
                .unwrap_or_else(|| {
                    format!(
                        "declared direct {} -> {}",
                        predecessor,
                        resource.resource_id()
                    )
                });
            add_ordering_edge(
                &mut proposed,
                predecessor.as_str(),
                resource.resource_id().as_str(),
                source,
            );
        }
        if let Some(stage) = resource.stage() {
            phase_members
                .entry(stage)
                .or_default()
                .push((resource.resource_id().as_str(), resource.stage_source()));
        }
    }
    let phases = orchestration
        .stages()
        .iter()
        .map(|phase| (phase.id(), phase))
        .collect::<BTreeMap<_, _>>();
    for phase in orchestration.stages() {
        let mut ancestors = BTreeSet::new();
        let mut pending = phase.after().to_vec();
        while let Some(id) = pending.pop() {
            if ancestors.insert(id.clone()) {
                if let Some(predecessor) = phases.get(id.as_str()) {
                    pending.extend(predecessor.after().iter().cloned());
                }
            }
        }
        for ancestor in ancestors {
            let (Some(inputs), Some(outputs)) = (
                phase_members.get(ancestor.as_str()),
                phase_members.get(phase.id()),
            ) else {
                continue;
            };
            for (from, from_source) in inputs {
                for (to, to_source) in outputs {
                    let phase_path = declared_phase_path(&phases, &ancestor, phase.id())
                        .unwrap_or_else(|| vec![ancestor.clone(), phase.id().to_owned()]);
                    add_ordering_edge(
                        &mut proposed,
                        from,
                        to,
                        format!(
                            "declared phase path {}; membership {} -> {}",
                            phase_path.join(" -> "),
                            from_source.unwrap_or("<unspecified>"),
                            to_source.unwrap_or("<unspecified>")
                        ),
                    );
                }
            }
        }
    }

    let known_ids = known
        .variants()
        .map(|resource| resource.resource_id().as_str().to_owned())
        .collect::<Vec<_>>();
    let rebuild_graph = recorded_rebuild_graph(topology);
    let mut conflicts = Vec::new();
    for old_before in &known_ids {
        for old_after in &known_ids {
            if old_before == old_after {
                continue;
            }
            let (Some(old_sources), Some(proposed_sources)) = (
                ordering_path(&recorded, old_before, old_after),
                ordering_path(&proposed, old_after, old_before),
            ) else {
                continue;
            };
            let mut rebuild = rebuild_component(&rebuild_graph, old_before);
            rebuild.extend(rebuild_component(&rebuild_graph, old_after));
            conflicts.push(Diagnostic::RecordedOrderConflict {
                old_before: FullyQualifiedResourceId::parse(old_before)
                    .expect("Known ID is canonical"),
                old_after: FullyQualifiedResourceId::parse(old_after)
                    .expect("Known ID is canonical"),
                old_sources,
                proposed_before: FullyQualifiedResourceId::parse(old_after)
                    .expect("Known ID is canonical"),
                proposed_after: FullyQualifiedResourceId::parse(old_before)
                    .expect("Known ID is canonical"),
                proposed_sources,
                rebuild_set: rebuild
                    .into_iter()
                    .filter_map(|node| node.strip_prefix("resource:").map(ToOwned::to_owned))
                    .map(|id| {
                        FullyQualifiedResourceId::parse(&id).expect("recorded ID is canonical")
                    })
                    .collect(),
            });
        }
    }
    conflicts
}

fn declared_phase_path(
    phases: &BTreeMap<&str, &crate::domain::desired::ResolvedStage>,
    ancestor: &str,
    descendant: &str,
) -> Option<Vec<String>> {
    let mut pending =
        std::collections::VecDeque::from([(descendant.to_owned(), vec![descendant.to_owned()])]);
    let mut visited = BTreeSet::new();
    while let Some((current, path)) = pending.pop_front() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if current == ancestor {
            return Some(path.into_iter().rev().collect());
        }
        for predecessor in phases.get(current.as_str())?.after() {
            let mut next = path.clone();
            next.push(predecessor.clone());
            pending.push_back((predecessor.clone(), next));
        }
    }
    None
}

fn add_ordering_edge(edges: &mut OrderingEdges, from: &str, to: &str, source: String) {
    edges
        .entry(from.to_owned())
        .or_default()
        .entry(to.to_owned())
        .and_modify(|existing| {
            if source < *existing {
                *existing = source.clone();
            }
        })
        .or_insert(source);
}

fn ordering_path(edges: &OrderingEdges, start: &str, target: &str) -> Option<Vec<String>> {
    let mut pending = std::collections::VecDeque::from([(start.to_owned(), Vec::new())]);
    let mut visited = BTreeSet::from([start.to_owned()]);
    while let Some((node, path)) = pending.pop_front() {
        for (successor, source) in edges.get(&node).into_iter().flat_map(|edges| edges.iter()) {
            if !visited.insert(successor.clone()) {
                continue;
            }
            let mut next = path.clone();
            next.push(source.clone());
            if successor == target {
                return Some(next);
            }
            pending.push_back((successor.clone(), next));
        }
    }
    None
}

fn recorded_rebuild_graph(
    topology: &crate::domain::known::AppliedLifecycleTopology,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut graph = BTreeMap::<String, BTreeSet<String>>::new();
    let mut connect = |left: String, right: String| {
        graph.entry(left.clone()).or_default().insert(right.clone());
        graph.entry(right).or_default().insert(left);
    };
    for fact in topology
        .current_direct()
        .iter()
        .chain(topology.historical_direct())
    {
        connect(
            format!("resource:{}", fact.from),
            format!("resource:{}", fact.to),
        );
    }
    for barrier in topology
        .current_barriers()
        .iter()
        .chain(topology.historical_barriers())
    {
        let node = format!("barrier:{}", barrier.id);
        for member in &barrier.members {
            connect(node.clone(), format!("resource:{member}"));
        }
        for predecessor in &barrier.after {
            connect(node.clone(), format!("barrier:{predecessor}"));
        }
    }
    graph
}

fn rebuild_component(
    graph: &BTreeMap<String, BTreeSet<String>>,
    resource_id: &str,
) -> BTreeSet<String> {
    let start = format!("resource:{resource_id}");
    let mut pending = vec![start.clone()];
    let mut visited = BTreeSet::from([start]);
    while let Some(node) = pending.pop() {
        for neighbor in graph
            .get(&node)
            .into_iter()
            .flat_map(|neighbors| neighbors.iter())
        {
            if visited.insert(neighbor.clone()) {
                pending.push(neighbor.clone());
            }
        }
    }
    visited
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
    known: &KnownState,
) -> Vec<FullyQualifiedResourceId> {
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
    let roles = graph
        .actions
        .iter()
        .map(|node| (node.resource_id.clone(), node.role))
        .collect::<BTreeMap<_, _>>();
    let snapshot = graph.stale_topology_snapshot.clone();
    let mut projected = known.topology().clone();
    let mut blocked = Vec::new();
    for action_id in graph.schedule.clone() {
        let Some(node) = graph.actions.iter_mut().find(|node| node.id == action_id) else {
            continue;
        };
        if is_current(node.role) {
            for (from, to) in direct_facts
                .iter()
                .filter(|(_, to)| *to == node.resource_id)
            {
                let fact = AppliedDirectEdge {
                    from: from.clone(),
                    to: to.clone(),
                    origin: "depends_on",
                };
                if !projected.current_direct().contains(&fact)
                    && !projected.historical_direct().contains(&fact)
                {
                    node.topology_authority.current_direct.push(fact.clone());
                    node.topology_delta.add_current_direct.push(fact);
                }
            }
            if !select_stage_topology_publication(node, orchestration, resources, &projected) {
                blocked.push(node.resource_id.clone());
                continue;
            }
        } else {
            select_stale_topology_publication(node, &snapshot, &projected, &roles);
        }
        match projected.transition(&node.topology_authority, &node.topology_delta) {
            Ok(next) => projected = next,
            Err(_) => blocked.push(node.resource_id.clone()),
        }
    }
    blocked
}

/// Publishes stage facts only from actions that verified a member of that stage.
fn select_stage_topology_publication(
    node: &mut GraphAction,
    orchestration: &crate::domain::desired::ResolvedOrchestration,
    resources: &BTreeMap<
        FullyQualifiedResourceId,
        &crate::domain::desired::ResolvedResourceOrchestration,
    >,
    projected: &crate::domain::known::AppliedLifecycleTopology,
) -> bool {
    let resource_id = &node.resource_id;
    let Some(stage_id) = resources
        .get(resource_id)
        .and_then(|resource| resource.stage())
    else {
        return true;
    };
    let stages = orchestration
        .stages()
        .iter()
        .map(|stage| (stage.id().to_owned(), stage))
        .collect::<BTreeMap<_, _>>();
    let Some(stage) = stages.get(stage_id) else {
        return false;
    };
    let facts = projected
        .current_barriers()
        .iter()
        .map(|fact| (fact.id.clone(), (true, fact.clone())))
        .chain(
            projected
                .historical_barriers()
                .iter()
                .map(|fact| (fact.id.clone(), (false, fact.clone()))),
        )
        .collect::<BTreeMap<_, _>>();
    let id = canonical_phase_id(stage_id);
    let before = facts.get(&id).cloned();
    let mut ancestors = BTreeSet::new();
    let mut pending = stage.after().to_vec();
    while let Some(ancestor) = pending.pop() {
        if ancestors.insert(canonical_phase_id(&ancestor)) {
            if let Some(predecessor) = stages.get(&ancestor) {
                pending.extend(predecessor.after().iter().cloned());
            }
        }
    }
    let mut proved = ancestors.clone();
    if let Some((_, fact)) = &before {
        if !fact.members.contains(resource_id) {
            let mut pending = fact.after.clone();
            while let Some(predecessor) = pending.pop() {
                if !proved.insert(predecessor.clone()) {
                    continue;
                }
                let Some((_, recorded)) = facts.get(&predecessor) else {
                    return false;
                };
                pending.extend(recorded.after.iter().cloned());
            }
        }
    }
    if proved.iter().any(|ancestor| {
        facts.get(ancestor).is_some_and(|(_, fact)| {
            fact.members
                .iter()
                .any(|member| !resources.contains_key(member))
        })
    }) {
        return false;
    }
    let candidates = ancestors
        .into_iter()
        .filter(|ancestor| facts.contains_key(ancestor))
        .collect::<BTreeSet<_>>();
    let mut after = before
        .as_ref()
        .map(|(_, fact)| fact.after.iter().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    for candidate in &candidates {
        if after.contains(candidate) {
            continue;
        }
        if candidates
            .iter()
            .chain(after.iter())
            .any(|other| other != candidate && recorded_phase_reaches(&facts, candidate, other))
        {
            continue;
        }
        after.insert(candidate.clone());
    }
    let mut members = before
        .as_ref()
        .map(|(_, fact)| fact.members.clone())
        .unwrap_or_default();
    if !members.contains(resource_id) {
        members.push(resource_id.clone());
        members.sort();
    }
    let next = AppliedBarrier {
        id,
        after: after.into_iter().collect(),
        members,
        origin: "stage",
    };
    match before {
        None => {
            node.topology_authority.current_barriers.push(next.clone());
            node.topology_delta.add_current_barriers.push(next);
        }
        Some((is_current_array, previous)) if previous != next => {
            let advance = TopologyBarrierAdvance::new(previous.clone(), next)
                .expect("current output advances only its member and proved predecessors");
            if is_current_array {
                node.topology_authority.current_barriers.push(previous);
                node.topology_delta.advance_current_barriers.push(advance);
            } else {
                node.topology_authority.historical_barriers.push(previous);
                node.topology_delta
                    .advance_historical_barriers
                    .push(advance);
            }
        }
        Some(_) => {}
    }
    true
}

fn recorded_phase_reaches(
    facts: &BTreeMap<String, (bool, AppliedBarrier)>,
    ancestor: &str,
    descendant: &str,
) -> bool {
    let mut pending = vec![descendant.to_owned()];
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        if id == ancestor {
            return true;
        }
        if let Some((_, fact)) = facts.get(&id) {
            pending.extend(fact.after.iter().cloned());
        }
    }
    false
}

fn canonical_phase_id(id: &str) -> String {
    id.strip_prefix("stage/")
        .map(|_| id.to_owned())
        .unwrap_or_else(|| format!("stage/{id}"))
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
    node: &mut GraphAction,
    snapshot: &StaleTopologySnapshot,
    projected: &crate::domain::known::AppliedLifecycleTopology,
    roles: &BTreeMap<FullyQualifiedResourceId, ActionRole>,
) {
    for (is_current, edges) in [
        (true, &snapshot.current_direct),
        (false, &snapshot.historical_direct),
    ] {
        for edge in edges {
            // In a fully stale relation the successor owns the reversed removal.
            // An incident mixed fact has exactly one stale endpoint, which is its sole owner.
            let owner = if matches!(
                roles.get(&edge.to),
                Some(ActionRole::StaleRemove | ActionRole::ForgetMissing)
            ) {
                &edge.to
            } else {
                &edge.from
            };
            if owner != &node.resource_id {
                continue;
            }
            if is_current && projected.current_direct().contains(edge) {
                node.topology_authority.current_direct.push(edge.clone());
                node.topology_delta.remove_current_direct.push(edge.clone());
            } else if !is_current && projected.historical_direct().contains(edge) {
                node.topology_authority.historical_direct.push(edge.clone());
                node.topology_delta
                    .remove_historical_direct
                    .push(edge.clone());
            }
        }
    }
    let before = projected
        .current_barriers()
        .iter()
        .map(|fact| (fact.id.clone(), (true, fact.clone())))
        .chain(
            projected
                .historical_barriers()
                .iter()
                .map(|fact| (fact.id.clone(), (false, fact.clone()))),
        )
        .collect::<BTreeMap<_, _>>();
    let mut after = before.clone();
    let mut emptied = BTreeSet::new();
    for (id, (_, fact)) in &before {
        if !fact.members.contains(&node.resource_id) {
            continue;
        }
        let projected_fact = &mut after.get_mut(id).expect("selected barrier exists").1;
        projected_fact
            .members
            .retain(|member| member != &node.resource_id);
        if projected_fact.members.is_empty() {
            after.remove(id);
            emptied.insert(id.clone());
        }
    }
    if !emptied.is_empty() {
        for (_, fact) in after.values_mut() {
            fact.after
                .retain(|predecessor| !emptied.contains(predecessor));
        }
    }
    for (id, (is_current, original)) in before {
        match after.get(&id) {
            None => {
                if is_current {
                    node.topology_authority
                        .current_barriers
                        .push(original.clone());
                    node.topology_delta.remove_current_barriers.push(original);
                } else {
                    node.topology_authority
                        .historical_barriers
                        .push(original.clone());
                    node.topology_delta
                        .remove_historical_barriers
                        .push(original);
                }
            }
            Some((_, next)) if next != &original => {
                let replacement = TopologyBarrierReplacement::new(original.clone(), next.clone())
                    .expect("stale action only removes its member or emptied predecessors");
                if is_current {
                    node.topology_authority.current_barriers.push(original);
                    node.topology_delta
                        .replace_current_barriers
                        .push(replacement);
                } else {
                    node.topology_authority.historical_barriers.push(original);
                    node.topology_delta
                        .replace_historical_barriers
                        .push(replacement);
                }
            }
            Some(_) => {}
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
    for barrier in barriers
        .iter()
        .chain(stale_barriers)
        .filter(|barrier| !barrier.input.is_empty())
    {
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
    let phase_barriers = barriers
        .iter()
        .filter(|barrier| !barrier.id.starts_with("stale/"))
        .map(|barrier| (barrier.id.clone(), barrier))
        .collect::<BTreeMap<_, _>>();
    let mut phase_of_action = BTreeMap::<String, String>::new();
    let mut phase_remaining = BTreeMap::<String, usize>::new();
    let mut predecessor_remaining = BTreeMap::<String, usize>::new();
    let mut phase_successors = BTreeMap::<String, Vec<String>>::new();
    for (id, barrier) in &phase_barriers {
        phase_remaining.insert(id.clone(), barrier.output.len());
        predecessor_remaining.insert(id.clone(), barrier.after.len());
        for predecessor in &barrier.after {
            phase_successors
                .entry(predecessor.clone())
                .or_default()
                .push(id.clone());
        }
        for output in &barrier.output {
            phase_of_action.insert(output.clone(), id.clone());
            *incoming
                .get_mut(&FullyQualifiedResourceId::parse(output).expect("canonical action ID"))
                .expect("phase members name actions") += 1;
        }
    }
    let mut ready = incoming
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let mut ready_phases = predecessor_remaining
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(id, _)| id.clone())
        .collect::<std::collections::VecDeque<_>>();
    release_ready_phases(
        &mut ready_phases,
        &phase_barriers,
        &phase_remaining,
        &mut predecessor_remaining,
        &phase_successors,
        &mut incoming,
        &mut ready,
    );
    let mut schedule = Vec::new();
    while let Some(id) = ready.pop_first() {
        schedule.push(id.as_str().to_owned());
        if let Some(phase) = phase_of_action.get(id.as_str()) {
            let remaining = phase_remaining.get_mut(phase).expect("known phase");
            *remaining -= 1;
            if *remaining == 0 {
                for successor in phase_successors.get(phase).into_iter().flatten() {
                    let count = predecessor_remaining
                        .get_mut(successor)
                        .expect("known phase");
                    *count -= 1;
                    if *count == 0 {
                        ready_phases.push_back(successor.clone());
                    }
                }
                release_ready_phases(
                    &mut ready_phases,
                    &phase_barriers,
                    &phase_remaining,
                    &mut predecessor_remaining,
                    &phase_successors,
                    &mut incoming,
                    &mut ready,
                );
            }
        }
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

fn release_ready_phases(
    queue: &mut std::collections::VecDeque<String>,
    phases: &BTreeMap<String, &GraphBarrier>,
    remaining_actions: &BTreeMap<String, usize>,
    remaining_predecessors: &mut BTreeMap<String, usize>,
    successors: &BTreeMap<String, Vec<String>>,
    incoming: &mut BTreeMap<FullyQualifiedResourceId, usize>,
    ready: &mut BTreeSet<FullyQualifiedResourceId>,
) {
    while let Some(phase) = queue.pop_front() {
        let barrier = phases[&phase];
        for output in &barrier.output {
            let id = FullyQualifiedResourceId::parse(output).expect("canonical phase action");
            let count = incoming.get_mut(&id).expect("phase action exists");
            *count -= 1;
            if *count == 0 {
                ready.insert(id);
            }
        }
        if remaining_actions[&phase] == 0 {
            for successor in successors.get(&phase).into_iter().flatten() {
                let count = remaining_predecessors
                    .get_mut(successor)
                    .expect("known phase");
                *count -= 1;
                if *count == 0 {
                    queue.push_back(successor.clone());
                }
            }
        }
    }
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
    let all_barriers = known
        .topology()
        .current_barriers()
        .iter()
        .chain(known.topology().historical_barriers())
        .map(|fact| (fact.id.clone(), fact))
        .collect::<BTreeMap<_, _>>();
    let mut selected = all_barriers
        .values()
        .filter(|fact| fact.members.iter().any(stale))
        .map(|fact| fact.id.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let mut additions = BTreeSet::new();
        for fact in all_barriers.values() {
            if selected.contains(&fact.id) {
                additions.extend(fact.after.iter().cloned());
            } else if fact.after.iter().any(|after| selected.contains(after)) {
                additions.insert(fact.id.clone());
            }
        }
        let previous_len = selected.len();
        selected.extend(additions);
        if selected.len() == previous_len {
            break;
        }
    }
    let select_barriers = |facts: &[AppliedBarrier]| {
        facts
            .iter()
            .filter(|fact| selected.contains(&fact.id))
            .cloned()
            .collect::<Vec<_>>()
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
                result
                    .diagnostics
                    .push(blocking(&id, OrderingBlockReason::Cycle));
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

fn blocking(resource_id: &FullyQualifiedResourceId, reason: OrderingBlockReason) -> Diagnostic {
    Diagnostic::OrderingBlocked {
        resource_id: resource_id.clone(),
        reason,
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

    #[test]
    fn missing_direct_predecessor_reports_ordering_proof_failure() {
        let successor = resource("root/successor");
        let predecessor = FullyQualifiedResourceId::parse("root/predecessor").unwrap();
        let desired = desired(
            vec![successor.clone()],
            ResolvedOrchestration::new(
                vec![],
                vec![ResolvedResourceOrchestration::new(
                    successor.resource_id().clone(),
                    None,
                    vec![predecessor],
                )],
            ),
        );
        let result = schedule(
            &desired,
            &KnownState::empty(),
            &[PlannedAction::create_link(successor).into()],
        );
        assert!(matches!(
            result.diagnostics(),
            [Diagnostic::OrderingBlocked {
                reason: OrderingBlockReason::UnprovedPredecessor,
                ..
            }]
        ));
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
        assert_eq!(compact.barriers.len(), 2);
        let second = compact
            .barriers
            .iter()
            .find(|barrier| barrier.id == "root/second")
            .unwrap();
        assert_eq!(second.input, ["root/alpha", "root/beta"]);
        assert_eq!(second.output, ["root/gamma"]);
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

        let late = graph
            .barriers
            .iter()
            .find(|barrier| barrier.id == "root/z")
            .unwrap();
        assert_eq!(late.provenance, ["root/a", "root/z"]);
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
        assert!(graph.barriers.iter().any(|barrier| barrier.id == "root/z"));
    }

    #[test]
    fn each_successful_output_advances_exact_barrier_membership() {
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
        assert_eq!(publishers, ["root/input-a", "root/output-a"]);
        let advancers = graph
            .actions
            .iter()
            .filter(|action| !action.topology_delta.advance_current_barriers.is_empty())
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(advancers, ["root/input-b", "root/output-b"]);
        let mut topology = crate::domain::known::AppliedLifecycleTopology::default();
        for id in &graph.schedule {
            let node = graph
                .actions
                .iter()
                .find(|action| action.id == *id)
                .unwrap();
            topology = topology
                .transition(&node.topology_authority, &node.topology_delta)
                .unwrap();
        }
        assert_eq!(topology.current_barriers().len(), 2);
        assert_eq!(topology.current_barriers()[1].members.len(), 2);
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
        assert_eq!(graph.barriers.len(), 2);
        let second = graph
            .barriers
            .iter()
            .find(|barrier| barrier.id == "root/second")
            .unwrap();
        assert_eq!(second.input.len() + second.output.len(), 64);
        assert_eq!(graph.schedule.len(), 64);
    }

    #[test]
    fn empty_intermediate_phase_preserves_compact_schedule_reachability() {
        let a = resource("root/a");
        let c = resource("root/c");
        let desired = desired(
            vec![a.clone(), c.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/a".into(), vec![]),
                    ResolvedStage::new("stage/root/b".into(), vec!["stage/root/a".into()]),
                    ResolvedStage::new("stage/root/c".into(), vec!["stage/root/b".into()]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        a.resource_id().clone(),
                        Some("stage/root/a".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        c.resource_id().clone(),
                        Some("stage/root/c".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let graph = schedule(
            &desired,
            &KnownState::empty(),
            &[
                PlannedAction::create_link(c).into(),
                PlannedAction::create_link(a).into(),
            ],
        );
        assert!(graph.diagnostics().is_empty());
        let graph = graph.plan_graph();
        assert_eq!(graph.schedule, ["root/a", "root/c"]);
        assert!(graph.direct_edges.is_empty());
        assert_eq!(graph.barriers.len(), 3);
        let middle = graph
            .barriers
            .iter()
            .find(|barrier| barrier.id == "stage/root/b")
            .unwrap();
        assert!(middle.output.is_empty());
        assert_eq!(middle.after, ["stage/root/a"]);
        let late = graph
            .actions
            .iter()
            .find(|action| action.id == "root/c")
            .unwrap();
        assert_eq!(
            late.topology_delta.add_current_barriers[0].after,
            ["stage/root/a"]
        );
        assert!(graph.actions.iter().all(|action| {
            action
                .topology_delta
                .add_current_barriers
                .iter()
                .all(|fact| fact.id != "stage/root/b")
        }));
        let mut topology = crate::domain::known::AppliedLifecycleTopology::default();
        for id in &graph.schedule {
            let node = graph
                .actions
                .iter()
                .find(|action| action.id == *id)
                .unwrap();
            topology = topology
                .transition(&node.topology_authority, &node.topology_delta)
                .unwrap();
        }
        assert_eq!(topology.current_barriers().len(), 2);
    }

    #[test]
    fn branching_phases_join_after_both_outputs_without_pair_edges() {
        let left = resource("root/left");
        let right = resource("root/right");
        let join = resource("root/join");
        let desired = desired(
            vec![join.clone(), right.clone(), left.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/left".into(), vec![]),
                    ResolvedStage::new("stage/root/right".into(), vec![]),
                    ResolvedStage::new(
                        "stage/root/join".into(),
                        vec!["stage/root/left".into(), "stage/root/right".into()],
                    ),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        left.resource_id().clone(),
                        Some("stage/root/left".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        right.resource_id().clone(),
                        Some("stage/root/right".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        join.resource_id().clone(),
                        Some("stage/root/join".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let graph = schedule(
            &desired,
            &KnownState::empty(),
            &[
                PlannedAction::create_link(join).into(),
                PlannedAction::create_link(right).into(),
                PlannedAction::create_link(left).into(),
            ],
        )
        .plan_graph();
        assert_eq!(graph.schedule, ["root/left", "root/right", "root/join"]);
        assert!(graph.direct_edges.is_empty());
        let joined = graph
            .actions
            .iter()
            .find(|node| node.id == "root/join")
            .unwrap();
        assert_eq!(
            joined.topology_delta.add_current_barriers[0].after,
            ["stage/root/left", "stage/root/right"]
        );
    }

    #[test]
    fn unchanged_recorded_middle_keeps_both_independently_proved_predecessors() {
        let a = resource("root/a");
        let b = resource("root/b");
        let c = resource("root/c");
        let known = KnownState::new([crate::domain::known::KnownFileLink::from_resolved(&b)])
            .unwrap()
            .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
                vec![],
                vec![],
                vec![AppliedBarrier {
                    id: "stage/root/b".into(),
                    after: vec![],
                    members: vec![b.resource_id().clone()],
                    origin: "stage",
                }],
                vec![],
            ));
        let desired = desired(
            vec![a.clone(), b.clone(), c.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/a".into(), vec![]),
                    ResolvedStage::new("stage/root/b".into(), vec!["stage/root/a".into()]),
                    ResolvedStage::new("stage/root/c".into(), vec!["stage/root/b".into()]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        a.resource_id().clone(),
                        Some("stage/root/a".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        b.resource_id().clone(),
                        Some("stage/root/b".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        c.resource_id().clone(),
                        Some("stage/root/c".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let graph = schedule(
            &desired,
            &known,
            &[
                PlannedAction::create_link(c).into(),
                PlannedAction::create_link(a).into(),
            ],
        );
        assert!(graph.diagnostics().is_empty());
        let graph = graph.plan_graph();
        let late = graph
            .actions
            .iter()
            .find(|action| action.id == "root/c")
            .unwrap();
        assert_eq!(
            late.topology_delta.add_current_barriers[0].after,
            ["stage/root/a", "stage/root/b"]
        );
        assert!(graph.actions.iter().all(|action| {
            action
                .topology_delta
                .add_current_barriers
                .iter()
                .all(|fact| fact.id != "stage/root/b")
        }));
        let mut topology = known.topology().clone();
        for id in &graph.schedule {
            let node = graph
                .actions
                .iter()
                .find(|action| action.id == *id)
                .unwrap();
            topology = topology
                .transition(&node.topology_authority, &node.topology_delta)
                .unwrap();
        }
        assert_eq!(
            topology.current_barriers()[2].after,
            ["stage/root/a", "stage/root/b"]
        );
    }

    #[test]
    fn successful_middle_action_proves_chain_without_shortcut() {
        let a = resource("root/a");
        let b = resource("root/b");
        let c = resource("root/c");
        let desired = desired(
            vec![a.clone(), b.clone(), c.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/a".into(), vec![]),
                    ResolvedStage::new("stage/root/b".into(), vec!["stage/root/a".into()]),
                    ResolvedStage::new("stage/root/c".into(), vec!["stage/root/b".into()]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        a.resource_id().clone(),
                        Some("stage/root/a".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        b.resource_id().clone(),
                        Some("stage/root/b".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        c.resource_id().clone(),
                        Some("stage/root/c".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let graph = schedule(
            &desired,
            &KnownState::empty(),
            &[
                PlannedAction::create_link(c).into(),
                PlannedAction::create_link(b).into(),
                PlannedAction::create_link(a).into(),
            ],
        )
        .plan_graph();
        assert_eq!(graph.schedule, ["root/a", "root/b", "root/c"]);
        let late = graph
            .actions
            .iter()
            .find(|action| action.id == "root/c")
            .unwrap();
        assert_eq!(
            late.topology_delta.add_current_barriers[0].after,
            ["stage/root/b"]
        );
    }

    #[test]
    fn acted_on_recorded_member_advances_only_proved_relation() {
        let a = resource("root/a");
        let b = resource("root/b");
        let old_b = ResolvedFileLink::new(
            b.resource_id().clone(),
            ResolvedPath::new(std::env::temp_dir().join("loadout-action-graph/old-b")).unwrap(),
            b.target_path().clone(),
        )
        .unwrap();
        let known_b = crate::domain::known::KnownFileLink::from_resolved(&old_b);
        let known = KnownState::new([known_b.clone()]).unwrap().with_topology(
            crate::domain::known::AppliedLifecycleTopology::new(
                vec![],
                vec![],
                vec![AppliedBarrier {
                    id: "stage/root/b".into(),
                    after: vec![],
                    members: vec![b.resource_id().clone()],
                    origin: "stage",
                }],
                vec![],
            ),
        );
        let desired = desired(
            vec![a.clone(), b.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/a".into(), vec![]),
                    ResolvedStage::new("stage/root/b".into(), vec!["stage/root/a".into()]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        a.resource_id().clone(),
                        Some("stage/root/a".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        b.resource_id().clone(),
                        Some("stage/root/b".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let action_b = PlannedAction::replace_link(b, known_b).unwrap();
        let graph = schedule(
            &desired,
            &known,
            &[action_b.into(), PlannedAction::create_link(a).into()],
        );
        assert!(graph.diagnostics().is_empty());
        let graph = graph.plan_graph();
        let output = graph
            .actions
            .iter()
            .find(|node| node.id == "root/b")
            .unwrap();
        let advance = &output.topology_delta.advance_current_barriers[0];
        assert_eq!(advance.before.members, advance.after.members);
        assert!(advance.before.after.is_empty());
        assert_eq!(advance.after.after, ["stage/root/a"]);
        assert_eq!(
            output.topology_authority.current_barriers.as_slice(),
            std::slice::from_ref(&advance.before)
        );
    }

    #[test]
    fn stale_final_member_removes_surviving_successor_reference_atomically() {
        let a = resource("root/a");
        let c = resource("root/c");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&a),
            crate::domain::known::KnownFileLink::from_resolved(&c),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![],
            vec![],
            vec![
                AppliedBarrier {
                    id: "stage/root/a".into(),
                    after: vec![],
                    members: vec![a.resource_id().clone()],
                    origin: "stage",
                },
                AppliedBarrier {
                    id: "stage/root/c".into(),
                    after: vec!["stage/root/a".into()],
                    members: vec![c.resource_id().clone()],
                    origin: "stage",
                },
            ],
            vec![],
        ));
        let desired = desired(vec![c], ResolvedOrchestration::default());
        let graph = schedule(
            &desired,
            &known,
            &[
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(&a))
                    .into(),
            ],
        );
        assert!(graph.diagnostics().is_empty());
        let graph = graph.plan_graph();
        let action = &graph.actions[0];
        assert_eq!(action.topology_delta.remove_current_barriers.len(), 1);
        assert_eq!(action.topology_delta.replace_current_barriers.len(), 1);
        let cleanup = &action.topology_delta.replace_current_barriers[0];
        assert_eq!(cleanup.before.id, "stage/root/c");
        assert_eq!(cleanup.before.members, cleanup.after.members);
        assert!(cleanup.after.after.is_empty());
        let after = known
            .topology()
            .transition(&action.topology_authority, &action.topology_delta)
            .unwrap();
        assert_eq!(after.current_barriers().len(), 1);
        assert_eq!(after.current_barriers()[0].id, "stage/root/c");
    }

    #[test]
    fn mixed_current_and_stale_actions_use_exact_projected_barrier_facts() {
        let new = resource("root/a");
        let old = resource("root/x");
        let known = KnownState::new([crate::domain::known::KnownFileLink::from_resolved(&old)])
            .unwrap()
            .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
                vec![],
                vec![],
                vec![AppliedBarrier {
                    id: "stage/root/p".into(),
                    after: vec![],
                    members: vec![old.resource_id().clone()],
                    origin: "stage",
                }],
                vec![],
            ));
        let desired = desired(
            vec![new.clone()],
            ResolvedOrchestration::new(
                vec![ResolvedStage::new("stage/root/p".into(), vec![])],
                vec![ResolvedResourceOrchestration::new(
                    new.resource_id().clone(),
                    Some("stage/root/p".into()),
                    vec![],
                )],
            ),
        );
        let graph = schedule(
            &desired,
            &known,
            &[
                PlannedAction::create_link(new).into(),
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(
                    &old,
                ))
                .into(),
            ],
        );
        assert!(graph.diagnostics().is_empty());
        let graph = graph.plan_graph();
        assert_eq!(graph.schedule, ["root/a", "root/x"]);
        let add = graph
            .actions
            .iter()
            .find(|node| node.id == "root/a")
            .unwrap();
        let remove = graph
            .actions
            .iter()
            .find(|node| node.id == "root/x")
            .unwrap();
        assert_eq!(
            remove.topology_delta.replace_current_barriers[0].before,
            add.topology_delta.advance_current_barriers[0].after
        );
        let mut topology = known.topology().clone();
        for id in &graph.schedule {
            let node = graph.actions.iter().find(|node| node.id == *id).unwrap();
            topology = topology
                .transition(&node.topology_authority, &node.topology_delta)
                .unwrap();
        }
        assert_eq!(topology.current_barriers()[0].members.len(), 1);
        assert_eq!(topology.current_barriers()[0].members[0].as_str(), "root/a");
    }

    #[test]
    fn stale_snapshot_and_cleanup_follow_cross_array_barrier_reference() {
        let a = resource("root/a");
        let c = resource("root/c");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&a),
            crate::domain::known::KnownFileLink::from_resolved(&c),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![],
            vec![],
            vec![AppliedBarrier {
                id: "stage/root/a".into(),
                after: vec![],
                members: vec![a.resource_id().clone()],
                origin: "stage",
            }],
            vec![AppliedBarrier {
                id: "stage/root/c".into(),
                after: vec!["stage/root/a".into()],
                members: vec![c.resource_id().clone()],
                origin: "stage",
            }],
        ));
        let desired = desired(vec![c], ResolvedOrchestration::default());
        let graph = schedule(
            &desired,
            &known,
            &[
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(&a))
                    .into(),
            ],
        );
        assert!(graph.diagnostics().is_empty());
        let graph = graph.plan_graph();
        assert_eq!(graph.stale_topology_snapshot.historical_barriers.len(), 1);
        let stale = &graph.actions[0];
        assert_eq!(stale.topology_delta.remove_current_barriers.len(), 1);
        assert_eq!(stale.topology_delta.replace_historical_barriers.len(), 1);
        let after = known
            .topology()
            .transition(&stale.topology_authority, &stale.topology_delta)
            .unwrap();
        assert!(after.current_barriers().is_empty());
        assert!(after.historical_barriers()[0].after.is_empty());
    }

    #[test]
    fn mixed_stale_then_current_recreates_phase_from_exact_empty_projection() {
        let old = resource("root/a");
        let new = resource("root/z");
        let known = KnownState::new([crate::domain::known::KnownFileLink::from_resolved(&old)])
            .unwrap()
            .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
                vec![],
                vec![],
                vec![AppliedBarrier {
                    id: "stage/root/p".into(),
                    after: vec![],
                    members: vec![old.resource_id().clone()],
                    origin: "stage",
                }],
                vec![],
            ));
        let desired = desired(
            vec![new.clone()],
            ResolvedOrchestration::new(
                vec![ResolvedStage::new("stage/root/p".into(), vec![])],
                vec![ResolvedResourceOrchestration::new(
                    new.resource_id().clone(),
                    Some("stage/root/p".into()),
                    vec![],
                )],
            ),
        );
        let graph = schedule(
            &desired,
            &known,
            &[
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(
                    &old,
                ))
                .into(),
                PlannedAction::create_link(new).into(),
            ],
        );
        assert!(graph.diagnostics().is_empty());
        let graph = graph.plan_graph();
        assert_eq!(graph.schedule, ["root/a", "root/z"]);
        assert_eq!(
            graph.actions[0]
                .topology_delta
                .remove_current_barriers
                .len(),
            1
        );
        assert_eq!(
            graph.actions[1].topology_delta.add_current_barriers.len(),
            1
        );
        let mut topology = known.topology().clone();
        for id in &graph.schedule {
            let node = graph.actions.iter().find(|node| node.id == *id).unwrap();
            topology = topology
                .transition(&node.topology_authority, &node.topology_delta)
                .unwrap();
        }
        assert_eq!(topology.current_barriers()[0].members[0].as_str(), "root/z");
    }

    #[test]
    fn stale_recorded_predecessor_cannot_authorize_a_new_current_relation() {
        let stale = resource("root/x");
        let current = resource("root/c");
        let known = KnownState::new([crate::domain::known::KnownFileLink::from_resolved(&stale)])
            .unwrap()
            .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
                vec![],
                vec![],
                vec![AppliedBarrier {
                    id: "stage/root/a".into(),
                    after: vec![],
                    members: vec![stale.resource_id().clone()],
                    origin: "stage",
                }],
                vec![],
            ));
        let desired = desired(
            vec![current.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/a".into(), vec![]),
                    ResolvedStage::new("stage/root/c".into(), vec!["stage/root/a".into()]),
                ],
                vec![ResolvedResourceOrchestration::new(
                    current.resource_id().clone(),
                    Some("stage/root/c".into()),
                    vec![],
                )],
            ),
        );
        let result = schedule(
            &desired,
            &known,
            &[
                PlannedAction::create_link(current).into(),
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(
                    &stale,
                ))
                .into(),
            ],
        );
        assert!(result.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic,
            Diagnostic::OrderingBlocked {
                reason: OrderingBlockReason::UnprovedTopologyPublication,
                ..
            }
        )));
        assert!(result.into_actions(vec![]).is_empty());
    }

    #[test]
    fn stale_recorded_predecessor_reports_its_unproved_order() {
        let early = resource("root/early");
        let existing = resource("root/existing");
        let joining = resource("root/joining");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&early),
            crate::domain::known::KnownFileLink::from_resolved(&existing),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![],
            vec![],
            vec![
                AppliedBarrier {
                    id: "stage/root/early".into(),
                    after: vec![],
                    members: vec![early.resource_id().clone()],
                    origin: "stage",
                },
                AppliedBarrier {
                    id: "stage/root/late".into(),
                    after: vec!["stage/root/early".into()],
                    members: vec![existing.resource_id().clone()],
                    origin: "stage",
                },
            ],
            vec![],
        ));
        let desired = desired(
            vec![existing.clone(), joining.clone()],
            ResolvedOrchestration::new(
                vec![ResolvedStage::new("stage/root/late".into(), vec![])],
                vec![ResolvedResourceOrchestration::new(
                    joining.resource_id().clone(),
                    Some("stage/root/late".into()),
                    vec![],
                )],
            ),
        );
        let result = schedule(
            &desired,
            &known,
            &[
                PlannedAction::remove_link(crate::domain::known::KnownFileLink::from_resolved(
                    &early,
                ))
                .into(),
                PlannedAction::create_link(joining).into(),
            ],
        );
        assert!(result.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic,
            Diagnostic::OrderingBlocked {
                reason: OrderingBlockReason::UnprovedRecordedPredecessor,
                ..
            }
        )));
    }

    #[test]
    fn unchanged_recorded_predecessor_proves_inherited_relation_for_new_member() {
        let early = resource("root/early");
        let late = resource("root/late");
        let new = resource("root/new");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&early),
            crate::domain::known::KnownFileLink::from_resolved(&late),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![],
            vec![],
            vec![
                AppliedBarrier {
                    id: "stage/root/early".into(),
                    after: vec![],
                    members: vec![early.resource_id().clone()],
                    origin: "stage",
                },
                AppliedBarrier {
                    id: "stage/root/late".into(),
                    after: vec!["stage/root/early".into()],
                    members: vec![late.resource_id().clone()],
                    origin: "stage",
                },
            ],
            vec![],
        ));
        let desired = desired(
            vec![early.clone(), late.clone(), new.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/early".into(), vec![]),
                    ResolvedStage::new("stage/root/late".into(), vec![]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        early.resource_id().clone(),
                        Some("stage/root/early".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        late.resource_id().clone(),
                        Some("stage/root/late".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        new.resource_id().clone(),
                        Some("stage/root/late".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let result = schedule(&desired, &known, &[PlannedAction::create_link(new).into()]);
        assert!(result.diagnostics().is_empty());
        let graph = result.plan_graph();
        assert_eq!(graph.schedule, ["root/new"]);
        let advance = &graph.actions[0].topology_delta.advance_current_barriers[0];
        assert_eq!(advance.after.after, ["stage/root/early"]);
        assert_eq!(advance.after.members.len(), 2);
        assert!(graph.direct_edges.is_empty());
    }

    #[test]
    fn current_recorded_predecessor_action_runs_before_new_member_even_without_desired_after() {
        let early = resource("root/z-early");
        let late = resource("root/l-late");
        let new = resource("root/a-new");
        let old_early = ResolvedFileLink::new(
            early.resource_id().clone(),
            ResolvedPath::new(std::env::temp_dir().join("loadout-action-graph/old-early")).unwrap(),
            early.target_path().clone(),
        )
        .unwrap();
        let known_early = crate::domain::known::KnownFileLink::from_resolved(&old_early);
        let known = KnownState::new([
            known_early.clone(),
            crate::domain::known::KnownFileLink::from_resolved(&late),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![],
            vec![],
            vec![
                AppliedBarrier {
                    id: "stage/root/early".into(),
                    after: vec![],
                    members: vec![early.resource_id().clone()],
                    origin: "stage",
                },
                AppliedBarrier {
                    id: "stage/root/late".into(),
                    after: vec!["stage/root/early".into()],
                    members: vec![late.resource_id().clone()],
                    origin: "stage",
                },
            ],
            vec![],
        ));
        let desired = desired(
            vec![early.clone(), late.clone(), new.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/early".into(), vec![]),
                    ResolvedStage::new("stage/root/late".into(), vec![]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        early.resource_id().clone(),
                        Some("stage/root/early".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        late.resource_id().clone(),
                        Some("stage/root/late".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        new.resource_id().clone(),
                        Some("stage/root/late".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let replace = PlannedAction::replace_link(early, known_early).unwrap();
        let graph = schedule(
            &desired,
            &known,
            &[PlannedAction::create_link(new).into(), replace.into()],
        );
        assert!(graph.diagnostics().is_empty());
        let graph = graph.plan_graph();
        assert_eq!(graph.schedule, ["root/z-early", "root/a-new"]);
        let late_barrier = graph
            .barriers
            .iter()
            .find(|barrier| barrier.id == "stage/root/late")
            .unwrap();
        assert_eq!(late_barrier.after, Vec::<String>::new());
        assert_eq!(late_barrier.input, ["root/z-early"]);
        assert!(late_barrier.provenance.contains(&"stage/root/early".into()));
        let output = graph
            .actions
            .iter()
            .find(|node| node.id == "root/a-new")
            .unwrap();
        assert_eq!(
            output.topology_delta.advance_current_barriers[0]
                .after
                .after,
            ["stage/root/early"]
        );
    }

    #[test]
    fn inherited_order_traverses_recorded_intermediate_phase() {
        let early = resource("root/z-early");
        let middle = resource("root/m-middle");
        let late = resource("root/l-late");
        let new = resource("root/a-new");
        let old_early = ResolvedFileLink::new(
            early.resource_id().clone(),
            ResolvedPath::new(
                std::env::temp_dir().join("loadout-action-graph/old-transitive-early"),
            )
            .unwrap(),
            early.target_path().clone(),
        )
        .unwrap();
        let known_early = crate::domain::known::KnownFileLink::from_resolved(&old_early);
        let known = KnownState::new([
            known_early.clone(),
            crate::domain::known::KnownFileLink::from_resolved(&middle),
            crate::domain::known::KnownFileLink::from_resolved(&late),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![],
            vec![],
            vec![
                AppliedBarrier {
                    id: "stage/root/early".into(),
                    after: vec![],
                    members: vec![early.resource_id().clone()],
                    origin: "stage",
                },
                AppliedBarrier {
                    id: "stage/root/late".into(),
                    after: vec!["stage/root/middle".into()],
                    members: vec![late.resource_id().clone()],
                    origin: "stage",
                },
                AppliedBarrier {
                    id: "stage/root/middle".into(),
                    after: vec!["stage/root/early".into()],
                    members: vec![middle.resource_id().clone()],
                    origin: "stage",
                },
            ],
            vec![],
        ));
        let desired = desired(
            vec![early.clone(), middle.clone(), late.clone(), new.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/early".into(), vec![]),
                    ResolvedStage::new("stage/root/late".into(), vec![]),
                    ResolvedStage::new("stage/root/middle".into(), vec![]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        early.resource_id().clone(),
                        Some("stage/root/early".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        middle.resource_id().clone(),
                        Some("stage/root/middle".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        late.resource_id().clone(),
                        Some("stage/root/late".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        new.resource_id().clone(),
                        Some("stage/root/late".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let replace = PlannedAction::replace_link(early, known_early).unwrap();
        let graph = schedule(
            &desired,
            &known,
            &[PlannedAction::create_link(new).into(), replace.into()],
        );
        assert!(graph.diagnostics().is_empty());
        let graph = graph.plan_graph();
        assert_eq!(graph.schedule, ["root/z-early", "root/a-new"]);
        let late_barrier = graph
            .barriers
            .iter()
            .find(|barrier| barrier.id == "stage/root/late")
            .unwrap();
        assert_eq!(late_barrier.input, ["root/z-early"]);
        assert!(
            late_barrier
                .provenance
                .contains(&"stage/root/middle".into())
        );
        assert_eq!(
            graph
                .actions
                .iter()
                .find(|node| node.id == "root/a-new")
                .unwrap()
                .topology_delta
                .advance_current_barriers[0]
                .after
                .after,
            ["stage/root/middle"]
        );
    }

    #[test]
    fn opposite_recorded_direct_order_blocks_even_when_both_resources_are_unchanged() {
        let a = resource("root/a");
        let b = resource("root/b");
        let c = resource("root/c");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&a),
            crate::domain::known::KnownFileLink::from_resolved(&b),
            crate::domain::known::KnownFileLink::from_resolved(&c),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![
                AppliedDirectEdge {
                    from: a.resource_id().clone(),
                    to: b.resource_id().clone(),
                    origin: "depends_on",
                },
                AppliedDirectEdge {
                    from: c.resource_id().clone(),
                    to: a.resource_id().clone(),
                    origin: "depends_on",
                },
            ],
            vec![],
            vec![],
            vec![],
        ));
        let desired = desired(
            vec![a.clone(), b.clone(), c.clone()],
            ResolvedOrchestration::new(
                vec![],
                vec![
                    ResolvedResourceOrchestration::new(
                        a.resource_id().clone(),
                        None,
                        vec![b.resource_id().clone()],
                    )
                    .with_provenance(
                        None,
                        vec![(
                            b.resource_id().clone(),
                            "profile=root; resource=a; field=depends_on".into(),
                        )],
                    ),
                    ResolvedResourceOrchestration::new(b.resource_id().clone(), None, vec![]),
                    ResolvedResourceOrchestration::new(c.resource_id().clone(), None, vec![]),
                ],
            ),
        );
        let result = schedule(&desired, &known, &[]);
        assert!(
            matches!(result.diagnostics(), [Diagnostic::RecordedOrderConflict {
            old_before, old_after, proposed_before, proposed_after, old_sources, proposed_sources, rebuild_set,
        }] if old_before.as_str() == "root/a" && old_after.as_str() == "root/b"
            && proposed_before.as_str() == "root/b" && proposed_after.as_str() == "root/a"
            && old_sources[0].contains("recorded current direct")
            && proposed_sources[0].contains("field=depends_on")
            && rebuild_set.iter().map(|id| id.as_str()).collect::<Vec<_>>() == ["root/a", "root/b", "root/c"])
        );
        assert!(result.into_actions(vec![]).is_empty());
    }

    #[test]
    fn renamed_phases_still_conflict_with_recorded_resource_order() {
        let a = resource("root/a");
        let b = resource("root/b");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&a),
            crate::domain::known::KnownFileLink::from_resolved(&b),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![],
            vec![],
            vec![
                AppliedBarrier {
                    id: "stage/root/old-a".into(),
                    after: vec![],
                    members: vec![a.resource_id().clone()],
                    origin: "stage",
                },
                AppliedBarrier {
                    id: "stage/root/old-b".into(),
                    after: vec!["stage/root/old-a".into()],
                    members: vec![b.resource_id().clone()],
                    origin: "stage",
                },
            ],
            vec![],
        ));
        let desired = desired(
            vec![a.clone(), b.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/new-a".into(), vec!["stage/root/new-b".into()]),
                    ResolvedStage::new("stage/root/new-b".into(), vec![]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        a.resource_id().clone(),
                        Some("stage/root/new-a".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        b.resource_id().clone(),
                        Some("stage/root/new-b".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let result = schedule(&desired, &known, &[]);
        assert!(
            matches!(result.diagnostics(), [Diagnostic::RecordedOrderConflict { old_sources, proposed_sources, rebuild_set, .. }]
            if old_sources[0].contains("old-a") && proposed_sources[0].contains("new-b")
            && rebuild_set.iter().map(|id| id.as_str()).collect::<Vec<_>>() == ["root/a", "root/b"])
        );
    }

    #[test]
    fn empty_declared_phase_exposes_conflict_with_historical_order() {
        let a = resource("root/a");
        let c = resource("root/c");
        let known = KnownState::new([
            crate::domain::known::KnownFileLink::from_resolved(&a),
            crate::domain::known::KnownFileLink::from_resolved(&c),
        ])
        .unwrap()
        .with_topology(crate::domain::known::AppliedLifecycleTopology::new(
            vec![],
            vec![AppliedDirectEdge {
                from: c.resource_id().clone(),
                to: a.resource_id().clone(),
                origin: "depends_on",
            }],
            vec![],
            vec![],
        ));
        let desired = desired(
            vec![a.clone(), c.clone()],
            ResolvedOrchestration::new(
                vec![
                    ResolvedStage::new("stage/root/a".into(), vec![]),
                    ResolvedStage::new("stage/root/b".into(), vec!["stage/root/a".into()]),
                    ResolvedStage::new("stage/root/c".into(), vec!["stage/root/b".into()]),
                ],
                vec![
                    ResolvedResourceOrchestration::new(
                        a.resource_id().clone(),
                        Some("stage/root/a".into()),
                        vec![],
                    ),
                    ResolvedResourceOrchestration::new(
                        c.resource_id().clone(),
                        Some("stage/root/c".into()),
                        vec![],
                    ),
                ],
            ),
        );
        let result = schedule(&desired, &known, &[]);
        assert!(
            matches!(result.diagnostics(), [Diagnostic::RecordedOrderConflict { old_sources, proposed_sources, .. }]
            if old_sources[0].contains("historical direct")
            && proposed_sources[0].contains("stage/root/a -> stage/root/b -> stage/root/c"))
        );
    }
}
