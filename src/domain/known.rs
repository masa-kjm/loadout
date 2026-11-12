//! Verified historical file-link facts owned by the state repository.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::domain::directory_link::ResolvedDirectoryLink;
use crate::domain::file_copy::{ContentFingerprint, ResolvedFileCopy};
use crate::domain::file_link::{LinkTarget, ResolvedFileLink};
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::paths::ResolvedPath;

/// A canonical direct prerequisite retained only after its endpoint effects commit.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AppliedDirectEdge {
    pub(crate) from: FullyQualifiedResourceId,
    pub(crate) to: FullyQualifiedResourceId,
    pub(crate) origin: &'static str,
}

/// One compact stage barrier fact. Membership is resource identity, never action identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AppliedBarrier {
    pub(crate) id: String,
    pub(crate) after: Vec<String>,
    pub(crate) members: Vec<FullyQualifiedResourceId>,
    pub(crate) origin: &'static str,
}

/// Immutable current and historical ordering facts owned by Known state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AppliedLifecycleTopology {
    current_direct: Vec<AppliedDirectEdge>,
    historical_direct: Vec<AppliedDirectEdge>,
    current_barriers: Vec<AppliedBarrier>,
    historical_barriers: Vec<AppliedBarrier>,
}

/// The immutable subset of applied topology that a single stale plan may use.
///
/// This is deliberately a value snapshot. Later declaration changes cannot add an
/// ordering relation to an already selected stale action.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StaleTopologySnapshot {
    pub(crate) current_direct: Vec<AppliedDirectEdge>,
    pub(crate) historical_direct: Vec<AppliedDirectEdge>,
    pub(crate) current_barriers: Vec<AppliedBarrier>,
    pub(crate) historical_barriers: Vec<AppliedBarrier>,
}

/// An exact barrier projection selected by the planner for one successful action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TopologyBarrierReplacement {
    pub(crate) before: AppliedBarrier,
    pub(crate) after: AppliedBarrier,
}

/// Exact topology facts an action is permitted to publish or remove after verification.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TopologyAuthority {
    pub(crate) current_direct: Vec<AppliedDirectEdge>,
    pub(crate) historical_direct: Vec<AppliedDirectEdge>,
    pub(crate) current_barriers: Vec<AppliedBarrier>,
    pub(crate) historical_barriers: Vec<AppliedBarrier>,
}

/// Planner-selected, action-local topology change. The state boundary applies no inferred facts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TopologyDelta {
    pub(crate) add_current_direct: Vec<AppliedDirectEdge>,
    pub(crate) remove_current_direct: Vec<AppliedDirectEdge>,
    pub(crate) add_historical_direct: Vec<AppliedDirectEdge>,
    pub(crate) remove_historical_direct: Vec<AppliedDirectEdge>,
    pub(crate) add_current_barriers: Vec<AppliedBarrier>,
    pub(crate) remove_current_barriers: Vec<AppliedBarrier>,
    pub(crate) add_historical_barriers: Vec<AppliedBarrier>,
    pub(crate) remove_historical_barriers: Vec<AppliedBarrier>,
    pub(crate) replace_current_barriers: Vec<TopologyBarrierReplacement>,
    pub(crate) replace_historical_barriers: Vec<TopologyBarrierReplacement>,
}

impl AppliedLifecycleTopology {
    /// Constructs an immutable topology from already canonical facts.
    pub(crate) fn new(
        current_direct: Vec<AppliedDirectEdge>,
        historical_direct: Vec<AppliedDirectEdge>,
        current_barriers: Vec<AppliedBarrier>,
        historical_barriers: Vec<AppliedBarrier>,
    ) -> Self {
        Self {
            current_direct,
            historical_direct,
            current_barriers,
            historical_barriers,
        }
    }

    /// Builds topology only when every persisted fact is canonical and disjoint by source array.
    pub(crate) fn try_new(
        current_direct: Vec<AppliedDirectEdge>,
        historical_direct: Vec<AppliedDirectEdge>,
        current_barriers: Vec<AppliedBarrier>,
        historical_barriers: Vec<AppliedBarrier>,
    ) -> Result<Self, TopologyTransitionError> {
        let topology = Self::new(
            current_direct,
            historical_direct,
            current_barriers,
            historical_barriers,
        );
        topology.validate()?;
        Ok(topology)
    }

    /// Validates the complete immutable topology before it is trusted for planning or publication.
    pub(crate) fn validate(&self) -> Result<(), TopologyTransitionError> {
        validate_direct_array(&self.current_direct)?;
        validate_direct_array(&self.historical_direct)?;
        validate_barrier_array(&self.current_barriers)?;
        validate_barrier_array(&self.historical_barriers)?;
        if self
            .current_direct
            .iter()
            .any(|fact| self.historical_direct.contains(fact))
        {
            return Err(TopologyTransitionError::CrossArrayDirectFact);
        }
        if self.current_barriers.iter().any(|current| {
            self.historical_barriers
                .iter()
                .any(|historical| historical.id == current.id)
        }) {
            return Err(TopologyTransitionError::CrossArrayBarrierId);
        }
        validate_barrier_graph(&self.current_barriers, &self.historical_barriers)?;
        Ok(())
    }

    /// Applies exactly one planner-authorized delta without deriving, merging, or reordering facts.
    pub(crate) fn transition(
        &self,
        authority: &TopologyAuthority,
        delta: &TopologyDelta,
    ) -> Result<Self, TopologyTransitionError> {
        self.validate()?;
        authority.validate()?;
        delta.validate(authority)?;

        let mut next = self.clone();
        apply_direct_delta(
            &mut next.current_direct,
            &delta.add_current_direct,
            &delta.remove_current_direct,
        )?;
        apply_direct_delta(
            &mut next.historical_direct,
            &delta.add_historical_direct,
            &delta.remove_historical_direct,
        )?;
        apply_barrier_delta(
            &mut next.current_barriers,
            &delta.add_current_barriers,
            &delta.remove_current_barriers,
            &delta.replace_current_barriers,
        )?;
        apply_barrier_delta(
            &mut next.historical_barriers,
            &delta.add_historical_barriers,
            &delta.remove_historical_barriers,
            &delta.replace_historical_barriers,
        )?;
        next.validate()?;
        Ok(next)
    }
    pub(crate) fn current_direct(&self) -> &[AppliedDirectEdge] {
        &self.current_direct
    }
    pub(crate) fn historical_direct(&self) -> &[AppliedDirectEdge] {
        &self.historical_direct
    }
    pub(crate) fn current_barriers(&self) -> &[AppliedBarrier] {
        &self.current_barriers
    }
    pub(crate) fn historical_barriers(&self) -> &[AppliedBarrier] {
        &self.historical_barriers
    }
}

impl TopologyBarrierReplacement {
    /// Creates one exact, shrinking barrier-membership projection.
    pub(crate) fn new(
        before: AppliedBarrier,
        after: AppliedBarrier,
    ) -> Result<Self, TopologyTransitionError> {
        if before.id != after.id {
            return Err(TopologyTransitionError::ReplacementChangesBarrierId);
        }
        if before.after != after.after {
            return Err(TopologyTransitionError::ReplacementChangesBarrierAfter);
        }
        if before.origin != after.origin {
            return Err(TopologyTransitionError::ReplacementChangesBarrierOrigin);
        }
        validate_barrier(&before)?;
        validate_barrier(&after)?;
        if after.members.len() >= before.members.len()
            || !after
                .members
                .iter()
                .all(|member| before.members.contains(member))
        {
            return Err(TopologyTransitionError::ReplacementDoesNotShrinkMembers);
        }
        Ok(Self { before, after })
    }

    fn validate(&self) -> Result<(), TopologyTransitionError> {
        Self::new(self.before.clone(), self.after.clone()).map(|_| ())
    }
}

impl TopologyAuthority {
    /// Validates canonical exact facts without interpreting their graph meaning.
    pub(crate) fn validate(&self) -> Result<(), TopologyTransitionError> {
        validate_direct_array(&self.current_direct)?;
        validate_direct_array(&self.historical_direct)?;
        validate_barrier_array(&self.current_barriers)?;
        validate_barrier_array(&self.historical_barriers)?;
        if self
            .current_direct
            .iter()
            .any(|fact| self.historical_direct.contains(fact))
        {
            return Err(TopologyTransitionError::CrossArrayDirectFact);
        }
        if self.current_barriers.iter().any(|current| {
            self.historical_barriers
                .iter()
                .any(|historical| historical.id == current.id)
        }) {
            return Err(TopologyTransitionError::CrossArrayBarrierId);
        }
        Ok(())
    }
}

impl TopologyDelta {
    /// Validates the exact delta shape and that every change is authorized by the selected facts.
    pub(crate) fn validate(
        &self,
        authority: &TopologyAuthority,
    ) -> Result<(), TopologyTransitionError> {
        validate_direct_array(&self.add_current_direct)?;
        validate_direct_array(&self.remove_current_direct)?;
        validate_direct_array(&self.add_historical_direct)?;
        validate_direct_array(&self.remove_historical_direct)?;
        validate_barrier_array(&self.add_current_barriers)?;
        validate_barrier_array(&self.remove_current_barriers)?;
        validate_barrier_array(&self.add_historical_barriers)?;
        validate_barrier_array(&self.remove_historical_barriers)?;
        validate_replacements(&self.replace_current_barriers)?;
        validate_replacements(&self.replace_historical_barriers)?;

        require_authorized(&self.add_current_direct, &authority.current_direct)?;
        require_authorized(&self.remove_current_direct, &authority.current_direct)?;
        require_authorized(&self.add_historical_direct, &authority.historical_direct)?;
        require_authorized(&self.remove_historical_direct, &authority.historical_direct)?;
        require_authorized(&self.add_current_barriers, &authority.current_barriers)?;
        require_authorized(&self.remove_current_barriers, &authority.current_barriers)?;
        require_authorized(
            &self.add_historical_barriers,
            &authority.historical_barriers,
        )?;
        require_authorized(
            &self.remove_historical_barriers,
            &authority.historical_barriers,
        )?;
        require_authorized(
            &self
                .replace_current_barriers
                .iter()
                .map(|replacement| replacement.before.clone())
                .collect::<Vec<_>>(),
            &authority.current_barriers,
        )?;
        require_authorized(
            &self
                .replace_historical_barriers
                .iter()
                .map(|replacement| replacement.before.clone())
                .collect::<Vec<_>>(),
            &authority.historical_barriers,
        )?;
        reject_overlapping_delta_entries(self)
    }
}

/// A malformed topology, authority, or requested immutable transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TopologyTransitionError {
    NonCanonicalDirectFacts,
    NonCanonicalBarrierFacts,
    InvalidDirectOrigin,
    SelfDirectEdge,
    InvalidBarrierId,
    InvalidBarrierAfterId,
    InvalidBarrierOrigin,
    SelfBarrierDependency,
    EmptyBarrierMembers,
    CrossArrayDirectFact,
    CrossArrayBarrierId,
    ReplacementChangesBarrierId,
    ReplacementChangesBarrierAfter,
    ReplacementChangesBarrierOrigin,
    ReplacementDoesNotShrinkMembers,
    UnauthorizedFact,
    OverlappingDeltaFact,
    AbsentBeforeFact,
    ExistingAddedFact,
    MissingBarrierPredecessor,
    CyclicBarrierDependencies,
}

impl fmt::Display for TopologyTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NonCanonicalDirectFacts => {
                "topology direct facts must be sorted and duplicate-free"
            }
            Self::NonCanonicalBarrierFacts => {
                "topology barrier facts must be sorted and duplicate-free"
            }
            Self::InvalidDirectOrigin => "topology direct facts must use the depends_on origin",
            Self::SelfDirectEdge => "topology direct facts must not be self edges",
            Self::InvalidBarrierId => "topology barrier IDs must use the stage/ namespace",
            Self::InvalidBarrierAfterId => {
                "topology barrier predecessors must use the stage/ namespace"
            }
            Self::InvalidBarrierOrigin => "topology barriers must use the stage origin",
            Self::SelfBarrierDependency => "topology barriers must not depend on themselves",
            Self::EmptyBarrierMembers => "topology barriers must retain at least one member",
            Self::CrossArrayDirectFact => {
                "an exact topology direct fact cannot be current and historical"
            }
            Self::CrossArrayBarrierId => "a topology barrier ID cannot be current and historical",
            Self::ReplacementChangesBarrierId => {
                "a topology barrier replacement must retain its barrier ID"
            }
            Self::ReplacementChangesBarrierAfter => {
                "a topology barrier replacement must retain its exact predecessors"
            }
            Self::ReplacementChangesBarrierOrigin => {
                "a topology barrier replacement must retain its origin"
            }
            Self::ReplacementDoesNotShrinkMembers => {
                "a topology barrier replacement must retain a strict member subset"
            }
            Self::UnauthorizedFact => "a topology delta names a fact outside its authority",
            Self::OverlappingDeltaFact => "a topology delta names one fact more than once",
            Self::AbsentBeforeFact => "a topology delta removes or replaces an absent exact fact",
            Self::ExistingAddedFact => "a topology delta adds an already present fact",
            Self::MissingBarrierPredecessor => "a topology barrier names an absent predecessor",
            Self::CyclicBarrierDependencies => "topology barrier dependencies must be acyclic",
        })
    }
}

impl std::error::Error for TopologyTransitionError {}

fn validate_direct_array(facts: &[AppliedDirectEdge]) -> Result<(), TopologyTransitionError> {
    if facts.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(TopologyTransitionError::NonCanonicalDirectFacts);
    }
    for fact in facts {
        if fact.origin != "depends_on" {
            return Err(TopologyTransitionError::InvalidDirectOrigin);
        }
        if fact.from == fact.to {
            return Err(TopologyTransitionError::SelfDirectEdge);
        }
    }
    Ok(())
}

fn validate_barrier_array(facts: &[AppliedBarrier]) -> Result<(), TopologyTransitionError> {
    if facts.windows(2).any(|pair| pair[0].id >= pair[1].id) {
        return Err(TopologyTransitionError::NonCanonicalBarrierFacts);
    }
    facts.iter().try_for_each(validate_barrier)
}

fn validate_barrier(fact: &AppliedBarrier) -> Result<(), TopologyTransitionError> {
    if !is_stage_id(&fact.id) {
        return Err(TopologyTransitionError::InvalidBarrierId);
    }
    if fact.origin != "stage" {
        return Err(TopologyTransitionError::InvalidBarrierOrigin);
    }
    if fact.after.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(TopologyTransitionError::NonCanonicalBarrierFacts);
    }
    if fact.members.is_empty() {
        return Err(TopologyTransitionError::EmptyBarrierMembers);
    }
    if fact.members.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(TopologyTransitionError::NonCanonicalBarrierFacts);
    }
    for predecessor in &fact.after {
        if !is_stage_id(predecessor) {
            return Err(TopologyTransitionError::InvalidBarrierAfterId);
        }
        if predecessor == &fact.id {
            return Err(TopologyTransitionError::SelfBarrierDependency);
        }
    }
    Ok(())
}

fn validate_barrier_graph(
    current: &[AppliedBarrier],
    historical: &[AppliedBarrier],
) -> Result<(), TopologyTransitionError> {
    let barriers = current.iter().chain(historical.iter()).collect::<Vec<_>>();
    for barrier in &barriers {
        if barrier
            .after
            .iter()
            .any(|id| !barriers.iter().any(|candidate| candidate.id == *id))
        {
            return Err(TopologyTransitionError::MissingBarrierPredecessor);
        }
    }
    fn visits_cycle(
        id: &str,
        barriers: &[&AppliedBarrier],
        visiting: &mut Vec<String>,
        done: &mut Vec<String>,
    ) -> bool {
        if visiting.iter().any(|entry| entry == id) {
            return true;
        }
        if done.iter().any(|entry| entry == id) {
            return false;
        }
        visiting.push(id.to_owned());
        let cyclic = barriers
            .iter()
            .find(|barrier| barrier.id == id)
            .is_some_and(|barrier| {
                barrier
                    .after
                    .iter()
                    .any(|before| visits_cycle(before, barriers, visiting, done))
            });
        visiting.pop();
        done.push(id.to_owned());
        cyclic
    }
    let mut visiting = Vec::new();
    let mut done = Vec::new();
    if barriers
        .iter()
        .any(|barrier| visits_cycle(&barrier.id, &barriers, &mut visiting, &mut done))
    {
        return Err(TopologyTransitionError::CyclicBarrierDependencies);
    }
    Ok(())
}

fn is_stage_id(id: &str) -> bool {
    id.strip_prefix("stage/")
        .is_some_and(|suffix| FullyQualifiedResourceId::parse(suffix).is_ok())
}

fn validate_replacements(
    replacements: &[TopologyBarrierReplacement],
) -> Result<(), TopologyTransitionError> {
    if replacements
        .windows(2)
        .any(|pair| pair[0].before.id >= pair[1].before.id)
    {
        return Err(TopologyTransitionError::NonCanonicalBarrierFacts);
    }
    replacements
        .iter()
        .try_for_each(TopologyBarrierReplacement::validate)
}

fn require_authorized<T: PartialEq>(
    facts: &[T],
    authority: &[T],
) -> Result<(), TopologyTransitionError> {
    if facts.iter().all(|fact| authority.contains(fact)) {
        Ok(())
    } else {
        Err(TopologyTransitionError::UnauthorizedFact)
    }
}

fn reject_overlapping_delta_entries(delta: &TopologyDelta) -> Result<(), TopologyTransitionError> {
    let mut direct = BTreeSet::new();
    for fact in delta
        .add_current_direct
        .iter()
        .chain(&delta.remove_current_direct)
        .chain(&delta.add_historical_direct)
        .chain(&delta.remove_historical_direct)
    {
        if !direct.insert(fact) {
            return Err(TopologyTransitionError::OverlappingDeltaFact);
        }
    }
    let mut barriers = BTreeSet::new();
    for fact in delta
        .add_current_barriers
        .iter()
        .chain(&delta.remove_current_barriers)
        .chain(&delta.add_historical_barriers)
        .chain(&delta.remove_historical_barriers)
        .chain(
            delta
                .replace_current_barriers
                .iter()
                .map(|item| &item.before),
        )
        .chain(
            delta
                .replace_historical_barriers
                .iter()
                .map(|item| &item.before),
        )
    {
        if !barriers.insert(&fact.id) {
            return Err(TopologyTransitionError::OverlappingDeltaFact);
        }
    }
    Ok(())
}

fn apply_direct_delta(
    facts: &mut Vec<AppliedDirectEdge>,
    additions: &[AppliedDirectEdge],
    removals: &[AppliedDirectEdge],
) -> Result<(), TopologyTransitionError> {
    for fact in removals {
        let Some(position) = facts.iter().position(|candidate| candidate == fact) else {
            return Err(TopologyTransitionError::AbsentBeforeFact);
        };
        facts.remove(position);
    }
    for fact in additions {
        if facts.contains(fact) {
            return Err(TopologyTransitionError::ExistingAddedFact);
        }
        facts.push(fact.clone());
    }
    Ok(())
}

fn apply_barrier_delta(
    facts: &mut Vec<AppliedBarrier>,
    additions: &[AppliedBarrier],
    removals: &[AppliedBarrier],
    replacements: &[TopologyBarrierReplacement],
) -> Result<(), TopologyTransitionError> {
    for fact in removals {
        let Some(position) = facts.iter().position(|candidate| candidate == fact) else {
            return Err(TopologyTransitionError::AbsentBeforeFact);
        };
        facts.remove(position);
    }
    for replacement in replacements {
        let Some(position) = facts
            .iter()
            .position(|candidate| candidate == &replacement.before)
        else {
            return Err(TopologyTransitionError::AbsentBeforeFact);
        };
        facts[position] = replacement.after.clone();
    }
    for fact in additions {
        if facts.iter().any(|candidate| candidate.id == fact.id) {
            return Err(TopologyTransitionError::ExistingAddedFact);
        }
        facts.push(fact.clone());
    }
    Ok(())
}

/// A previously verified file-link post-condition recorded in Known state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KnownFileLink {
    resource_id: FullyQualifiedResourceId,
    source_path: ResolvedPath,
    target_path: ResolvedPath,
    link_target: LinkTarget,
}

impl KnownFileLink {
    /// Validates a persisted Known file-link record before it can be trusted.
    pub(crate) fn new(
        resource_id: FullyQualifiedResourceId,
        source_path: ResolvedPath,
        target_path: ResolvedPath,
        link_target: LinkTarget,
    ) -> Result<Self, KnownFileLinkError> {
        if source_path == target_path {
            return Err(KnownFileLinkError::SourceEqualsTarget);
        }
        if link_target.as_path() != &source_path {
            return Err(KnownFileLinkError::LinkTargetDiffersFromSource);
        }

        Ok(Self {
            resource_id,
            source_path,
            target_path,
            link_target,
        })
    }

    /// Creates the Known record that becomes eligible only after verification.
    pub(crate) fn from_resolved(resource: &ResolvedFileLink) -> Self {
        Self {
            resource_id: resource.resource_id().clone(),
            source_path: resource.source_path().clone(),
            target_path: resource.target_path().clone(),
            link_target: resource.link_target().clone(),
        }
    }

    /// Returns the resource identity that owns this recorded post-condition.
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        &self.resource_id
    }

    /// Returns the source recorded after successful verification.
    pub(crate) fn source_path(&self) -> &ResolvedPath {
        &self.source_path
    }

    /// Returns the target recorded after successful verification.
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }

    /// Returns the exact link target whose Actual observation proves ownership.
    pub(crate) fn link_target(&self) -> &LinkTarget {
        &self.link_target
    }
}

/// The reason a persisted Known file-link fact violates the state contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KnownFileLinkError {
    SourceEqualsTarget,
    LinkTargetDiffersFromSource,
}

impl fmt::Display for KnownFileLinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceEqualsTarget => {
                formatter.write_str("a Known file-link source and target must not be the same path")
            }
            Self::LinkTargetDiffersFromSource => formatter
                .write_str("a Known file-link target must equal its recorded resolved source path"),
        }
    }
}

impl std::error::Error for KnownFileLinkError {}

/// A previously verified file-copy post-condition recorded in Known state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KnownFileCopy {
    resource_id: FullyQualifiedResourceId,
    source_path: ResolvedPath,
    target_path: ResolvedPath,
    content_fingerprint: ContentFingerprint,
}

impl KnownFileCopy {
    /// Validates a persisted Known file-copy record before it can be trusted.
    pub(crate) fn new(
        resource_id: FullyQualifiedResourceId,
        source_path: ResolvedPath,
        target_path: ResolvedPath,
        content_fingerprint: ContentFingerprint,
    ) -> Result<Self, KnownFileCopyError> {
        if source_path == target_path {
            return Err(KnownFileCopyError::SourceEqualsTarget);
        }
        Ok(Self {
            resource_id,
            source_path,
            target_path,
            content_fingerprint,
        })
    }

    /// Creates the Known record that becomes eligible only after verification.
    pub(crate) fn from_resolved(resource: &ResolvedFileCopy) -> Self {
        Self {
            resource_id: resource.resource_id().clone(),
            source_path: resource.source_path().clone(),
            target_path: resource.target_path().clone(),
            content_fingerprint: resource.source_content_fingerprint().clone(),
        }
    }

    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        &self.resource_id
    }
    pub(crate) fn source_path(&self) -> &ResolvedPath {
        &self.source_path
    }
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }
    pub(crate) fn content_fingerprint(&self) -> &ContentFingerprint {
        &self.content_fingerprint
    }
}

/// The reason a persisted Known file-copy fact violates the state contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KnownFileCopyError {
    SourceEqualsTarget,
}

impl fmt::Display for KnownFileCopyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Known file-copy source and target must not be the same path")
    }
}

impl std::error::Error for KnownFileCopyError {}

/// A closed verified resource effect in Known state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum KnownResource {
    FileLink(KnownFileLink),
    FileCopy(KnownFileCopy),
    DirectoryLink(KnownDirectoryLink),
}

impl KnownResource {
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        match self {
            Self::FileLink(resource) => resource.resource_id(),
            Self::FileCopy(resource) => resource.resource_id(),
            Self::DirectoryLink(resource) => resource.resource_id(),
        }
    }
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        match self {
            Self::FileLink(resource) => resource.target_path(),
            Self::FileCopy(resource) => resource.target_path(),
            Self::DirectoryLink(resource) => resource.target_path(),
        }
    }
}

impl From<KnownFileLink> for KnownResource {
    fn from(resource: KnownFileLink) -> Self {
        Self::FileLink(resource)
    }
}
impl From<KnownFileCopy> for KnownResource {
    fn from(resource: KnownFileCopy) -> Self {
        Self::FileCopy(resource)
    }
}

/// A previously verified directory-link post-condition recorded in Known state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KnownDirectoryLink {
    resource_id: FullyQualifiedResourceId,
    source_path: ResolvedPath,
    target_path: ResolvedPath,
    link_target: LinkTarget,
}

impl KnownDirectoryLink {
    pub(crate) fn new(
        resource_id: FullyQualifiedResourceId,
        source_path: ResolvedPath,
        target_path: ResolvedPath,
        link_target: LinkTarget,
    ) -> Result<Self, KnownDirectoryLinkError> {
        if source_path == target_path {
            return Err(KnownDirectoryLinkError::SourceEqualsTarget);
        }
        if link_target.as_path() != &source_path {
            return Err(KnownDirectoryLinkError::LinkTargetDiffersFromSource);
        }
        Ok(Self {
            resource_id,
            source_path,
            target_path,
            link_target,
        })
    }
    pub(crate) fn from_resolved(resource: &ResolvedDirectoryLink) -> Self {
        Self {
            resource_id: resource.resource_id().clone(),
            source_path: resource.source_path().clone(),
            target_path: resource.target_path().clone(),
            link_target: resource.link_target().clone(),
        }
    }
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        &self.resource_id
    }
    pub(crate) fn source_path(&self) -> &ResolvedPath {
        &self.source_path
    }
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }
    pub(crate) fn link_target(&self) -> &LinkTarget {
        &self.link_target
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KnownDirectoryLinkError {
    SourceEqualsTarget,
    LinkTargetDiffersFromSource,
}

impl fmt::Display for KnownDirectoryLinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceEqualsTarget => formatter
                .write_str("a Known directory-link source and target must not be the same path"),
            Self::LinkTargetDiffersFromSource => formatter.write_str(
                "a Known directory-link target must equal its recorded resolved source path",
            ),
        }
    }
}
impl std::error::Error for KnownDirectoryLinkError {}

impl From<KnownDirectoryLink> for KnownResource {
    fn from(resource: KnownDirectoryLink) -> Self {
        Self::DirectoryLink(resource)
    }
}

/// All verified historical resource facts, keyed by stable resource identity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct KnownState {
    resources: BTreeMap<FullyQualifiedResourceId, KnownResource>,
    topology: AppliedLifecycleTopology,
}

impl KnownState {
    /// Produces empty state for a machine with no successfully applied resources.
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// Builds state while validating resource-ID and target uniqueness.
    pub(crate) fn new(
        resources: impl IntoIterator<Item = impl Into<KnownResource>>,
    ) -> Result<Self, KnownStateError> {
        let mut known = Self::empty();
        let mut targets = BTreeMap::new();

        for resource in resources.into_iter().map(Into::into) {
            let resource_id = resource.resource_id().clone();
            let target_path = resource.target_path().clone();

            if known.resources.contains_key(&resource_id) {
                return Err(KnownStateError::DuplicateResourceId { resource_id });
            }
            if let Some(first_resource_id) =
                targets.insert(target_path.clone(), resource_id.clone())
            {
                return Err(KnownStateError::DuplicateTarget {
                    target_path,
                    first_resource_id,
                    duplicate_resource_id: resource_id,
                });
            }

            known.resources.insert(resource_id, resource);
        }

        Ok(known)
    }

    /// Looks up the verified historical fact for one resource identity.
    pub(crate) fn get(&self, resource_id: &FullyQualifiedResourceId) -> Option<&KnownFileLink> {
        match self.resources.get(resource_id) {
            Some(KnownResource::FileLink(resource)) => Some(resource),
            _ => None,
        }
    }

    /// Iterates over Known resources by fully qualified resource ID.
    pub(crate) fn resources(&self) -> impl ExactSizeIterator<Item = &KnownResource> {
        self.resources.values()
    }

    /// Returns the already committed topology; planning never derives historical order from Desired.
    pub(crate) fn topology(&self) -> &AppliedLifecycleTopology {
        &self.topology
    }

    /// Attaches already validated committed topology to this immutable Known input.
    pub(crate) fn with_topology(mut self, topology: AppliedLifecycleTopology) -> Self {
        self.topology = topology;
        self
    }

    /// Iterates over file-link facts for the legacy link-only implementation slice.
    pub(crate) fn file_links(&self) -> impl Iterator<Item = &KnownFileLink> {
        self.resources
            .values()
            .filter_map(|resource| match resource {
                KnownResource::FileLink(resource) => Some(resource),
                KnownResource::FileCopy(_) => None,
                KnownResource::DirectoryLink(_) => None,
            })
    }

    /// Iterates over every closed resource effect by fully qualified resource ID.
    pub(crate) fn variants(&self) -> impl ExactSizeIterator<Item = &KnownResource> {
        self.resources()
    }

    /// Looks up any typed verified historical fact for one resource identity.
    pub(crate) fn get_variant(
        &self,
        resource_id: &FullyQualifiedResourceId,
    ) -> Option<&KnownResource> {
        self.resources.get(resource_id)
    }

    /// Returns a new Known state with one verified resource fact inserted or updated.
    ///
    /// The state repository uses this only in the same atomic commit that records the corresponding action as succeeded.
    pub(crate) fn with_upserted(&self, resource: KnownFileLink) -> Result<Self, KnownStateError> {
        let mut resources = self.resources.clone();
        resources.insert(resource.resource_id().clone(), resource.into());
        Self::new(resources.into_values()).map(|state| state.with_topology(self.topology.clone()))
    }

    /// Returns a new Known state with one verified copy fact inserted or updated.
    pub(crate) fn with_upserted_copy(
        &self,
        resource: KnownFileCopy,
    ) -> Result<Self, KnownStateError> {
        let mut resources = self.resources.clone();
        resources.insert(resource.resource_id().clone(), resource.into());
        Self::new(resources.into_values()).map(|state| state.with_topology(self.topology.clone()))
    }

    pub(crate) fn with_upserted_directory_link(
        &self,
        resource: KnownDirectoryLink,
    ) -> Result<Self, KnownStateError> {
        let mut resources = self.resources.clone();
        resources.insert(resource.resource_id().clone(), resource.into());
        Self::new(resources.into_values()).map(|state| state.with_topology(self.topology.clone()))
    }

    pub(crate) fn with_removed_directory_link(
        &self,
        expected: &KnownDirectoryLink,
    ) -> Result<Self, KnownStateError> {
        if self.resources.get(expected.resource_id())
            != Some(&KnownResource::DirectoryLink(expected.clone()))
        {
            return Err(KnownStateError::ResourceMismatch {
                resource_id: expected.resource_id().clone(),
            });
        }
        let mut resources = self.resources.clone();
        resources.remove(expected.resource_id());
        Self::new(resources.into_values()).map(|state| state.with_topology(self.topology.clone()))
    }

    pub(crate) fn with_replaced_directory_identity(
        &self,
        expected_old: &KnownDirectoryLink,
        new_resource: KnownDirectoryLink,
    ) -> Result<Self, KnownStateError> {
        if self.resources.get(expected_old.resource_id())
            != Some(&KnownResource::DirectoryLink(expected_old.clone()))
        {
            return Err(KnownStateError::ResourceMismatch {
                resource_id: expected_old.resource_id().clone(),
            });
        }
        if self.resources.contains_key(new_resource.resource_id()) {
            return Err(KnownStateError::DestinationResourcePresent {
                resource_id: new_resource.resource_id().clone(),
            });
        }
        let mut resources = self.resources.clone();
        resources.remove(expected_old.resource_id());
        resources.insert(new_resource.resource_id().clone(), new_resource.into());
        Self::new(resources.into_values()).map(|state| state.with_topology(self.topology.clone()))
    }

    pub(crate) fn with_directory_identity_replaced_by_id(
        &self,
        old_resource_id: &FullyQualifiedResourceId,
        new_resource: KnownDirectoryLink,
    ) -> Result<Self, KnownStateError> {
        let Some(KnownResource::DirectoryLink(old_resource)) = self.resources.get(old_resource_id)
        else {
            return Err(KnownStateError::MissingResource {
                resource_id: old_resource_id.clone(),
            });
        };
        if old_resource.target_path() != new_resource.target_path()
            || self.resources.contains_key(new_resource.resource_id())
        {
            return Err(KnownStateError::ResourceMismatch {
                resource_id: old_resource_id.clone(),
            });
        }
        let mut resources = self.resources.clone();
        resources.remove(old_resource_id);
        resources.insert(new_resource.resource_id().clone(), new_resource.into());
        Self::new(resources.into_values()).map(|state| state.with_topology(self.topology.clone()))
    }

    /// Returns a new Known state without one exact previously verified fact.
    ///
    /// The state repository uses this only in the same atomic commit that marks a verified `remove_link` action as succeeded. Requiring the complete expected fact prevents an action record from deleting a newer or different Known resource under the same identity.
    pub(crate) fn with_removed(&self, expected: &KnownFileLink) -> Result<Self, KnownStateError> {
        let Some(actual) = self.resources.get(expected.resource_id()) else {
            return Err(KnownStateError::MissingResource {
                resource_id: expected.resource_id().clone(),
            });
        };
        if actual != &KnownResource::FileLink(expected.clone()) {
            return Err(KnownStateError::ResourceMismatch {
                resource_id: expected.resource_id().clone(),
            });
        }

        let mut resources = self.resources.clone();
        resources.remove(expected.resource_id());
        Self::new(resources.into_values()).map(|state| state.with_topology(self.topology.clone()))
    }

    /// Returns a new Known state without one exact previously verified copy fact.
    pub(crate) fn with_removed_copy(
        &self,
        expected: &KnownFileCopy,
    ) -> Result<Self, KnownStateError> {
        let Some(actual) = self.resources.get(expected.resource_id()) else {
            return Err(KnownStateError::MissingResource {
                resource_id: expected.resource_id().clone(),
            });
        };
        if actual != &KnownResource::FileCopy(expected.clone()) {
            return Err(KnownStateError::ResourceMismatch {
                resource_id: expected.resource_id().clone(),
            });
        }

        let mut resources = self.resources.clone();
        resources.remove(expected.resource_id());
        Self::new(resources.into_values()).map(|state| state.with_topology(self.topology.clone()))
    }

    /// Returns a new Known state without a stale resource whose target was freshly proven missing. The caller's operation record supplies the resource identity; no filesystem entry is removed for this transition.
    pub(crate) fn with_missing_resource_removed(
        &self,
        resource_id: &FullyQualifiedResourceId,
    ) -> Result<Self, KnownStateError> {
        if !self.resources.contains_key(resource_id) {
            return Err(KnownStateError::MissingResource {
                resource_id: resource_id.clone(),
            });
        }
        let mut resources = self.resources.clone();
        resources.remove(resource_id);
        Self::new(resources.into_values()).map(|state| state.with_topology(self.topology.clone()))
    }

    /// Atomically replaces one exact managed identity with a newly verified fact at the same target.
    pub(crate) fn with_replaced_identity(
        &self,
        expected_old: &KnownFileLink,
        new_resource: KnownFileLink,
    ) -> Result<Self, KnownStateError> {
        let Some(actual) = self.resources.get(expected_old.resource_id()) else {
            return Err(KnownStateError::MissingResource {
                resource_id: expected_old.resource_id().clone(),
            });
        };
        if actual != &KnownResource::FileLink(expected_old.clone()) {
            return Err(KnownStateError::ResourceMismatch {
                resource_id: expected_old.resource_id().clone(),
            });
        }
        if self.resources.contains_key(new_resource.resource_id()) {
            return Err(KnownStateError::DestinationResourcePresent {
                resource_id: new_resource.resource_id().clone(),
            });
        }
        let mut resources = self.resources.clone();
        resources.remove(expected_old.resource_id());
        resources.insert(new_resource.resource_id().clone(), new_resource.into());
        Self::new(resources.into_values()).map(|state| state.with_topology(self.topology.clone()))
    }
}

/// The reason Known state violates a global uniqueness invariant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum KnownStateError {
    DuplicateResourceId {
        resource_id: FullyQualifiedResourceId,
    },
    DuplicateTarget {
        target_path: ResolvedPath,
        first_resource_id: FullyQualifiedResourceId,
        duplicate_resource_id: FullyQualifiedResourceId,
    },
    MissingResource {
        resource_id: FullyQualifiedResourceId,
    },
    ResourceMismatch {
        resource_id: FullyQualifiedResourceId,
    },
    DestinationResourcePresent {
        resource_id: FullyQualifiedResourceId,
    },
}

impl fmt::Display for KnownStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateResourceId { resource_id } => {
                write!(formatter, "duplicate Known resource ID: {resource_id}")
            }
            Self::DuplicateTarget {
                target_path,
                first_resource_id,
                duplicate_resource_id,
            } => write!(
                formatter,
                "Known target {target_path} is recorded for both {first_resource_id} and {duplicate_resource_id}"
            ),
            Self::MissingResource { resource_id } => {
                write!(
                    formatter,
                    "Known state does not contain resource {resource_id}"
                )
            }
            Self::ResourceMismatch { resource_id } => write!(
                formatter,
                "Known state resource {resource_id} does not match the verified fact selected for removal"
            ),
            Self::DestinationResourcePresent { resource_id } => write!(
                formatter,
                "Known state already contains identity-handoff destination {resource_id}"
            ),
        }
    }
}

impl std::error::Error for KnownStateError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> ResolvedPath {
        ResolvedPath::new(std::env::temp_dir().join("loadout-domain-known").join(name)).unwrap()
    }

    fn known(id: &str, source: &str, target: &str) -> KnownFileLink {
        let source_path = path(source);
        KnownFileLink::new(
            FullyQualifiedResourceId::parse(id).unwrap(),
            source_path.clone(),
            path(target),
            LinkTarget::new(source_path),
        )
        .unwrap()
    }

    #[test]
    fn known_file_link_requires_the_recorded_link_target_to_equal_the_source() {
        let source = path("store/git/config");
        let other_source = path("store/git/other");

        assert_eq!(
            KnownFileLink::new(
                FullyQualifiedResourceId::parse("base/git").unwrap(),
                source,
                path("home/.gitconfig"),
                LinkTarget::new(other_source),
            )
            .unwrap_err(),
            KnownFileLinkError::LinkTargetDiffersFromSource
        );
    }

    #[test]
    fn known_file_link_rejects_a_record_that_would_target_its_source() {
        let source = path("store/git/config");

        assert_eq!(
            KnownFileLink::new(
                FullyQualifiedResourceId::parse("base/git").unwrap(),
                source.clone(),
                source.clone(),
                LinkTarget::new(source),
            )
            .unwrap_err(),
            KnownFileLinkError::SourceEqualsTarget
        );
    }

    #[test]
    fn known_state_rejects_duplicate_resource_ids_and_targets() {
        let duplicate_id = KnownState::new([
            known("base/git", "store/git/config", "home/.gitconfig"),
            known("base/git", "store/git/other", "home/.gitconfig-other"),
        ])
        .unwrap_err();
        assert!(matches!(
            duplicate_id,
            KnownStateError::DuplicateResourceId { .. }
        ));

        let duplicate_target = KnownState::new([
            known("base/git", "store/git/config", "home/.gitconfig"),
            known("base/zsh", "store/zshrc", "home/.gitconfig"),
        ])
        .unwrap_err();
        assert!(matches!(
            duplicate_target,
            KnownStateError::DuplicateTarget { .. }
        ));
    }

    #[test]
    fn known_state_keeps_copy_effects_typed_and_enforces_cross_effect_target_uniqueness() {
        let link = known("base/link", "store/link", "home/.config");
        let copy = KnownFileCopy::new(
            FullyQualifiedResourceId::parse("base/copy").unwrap(),
            path("store/copy"),
            path("home/.copyconfig"),
            ContentFingerprint::parse(format!("sha256:{}", "a".repeat(64))).unwrap(),
        )
        .unwrap();
        let state =
            KnownState::new([KnownResource::from(link), KnownResource::from(copy)]).unwrap();
        assert_eq!(state.variants().len(), 2);
        assert!(matches!(
            state.get_variant(&FullyQualifiedResourceId::parse("base/copy").unwrap()),
            Some(KnownResource::FileCopy(_))
        ));
    }

    #[test]
    fn known_state_upsert_retains_global_target_uniqueness_and_replaces_one_identity() {
        let existing = known("base/git", "store/git/config", "home/.gitconfig");
        let state = KnownState::new([existing.clone()]).unwrap();

        let inserted = state
            .with_upserted(known("base/zsh", "store/zshrc", "home/.zshrc"))
            .unwrap();
        assert_eq!(inserted.resources().len(), 2);

        assert!(matches!(
            state.with_upserted(known("base/zsh", "store/zshrc", "home/.gitconfig")),
            Err(KnownStateError::DuplicateTarget { .. })
        ));
        let updated = state
            .with_upserted(known("base/git", "store/git/next", "home/.gitconfig"))
            .unwrap();
        assert_eq!(updated.resources().len(), 1);
        assert_eq!(
            updated
                .get(&FullyQualifiedResourceId::parse("base/git").unwrap())
                .unwrap()
                .source_path(),
            &path("store/git/next")
        );
    }

    #[test]
    fn known_state_removal_requires_the_exact_verified_fact() {
        let existing = known("base/git", "store/git/config", "home/.gitconfig");
        let state = KnownState::new([existing.clone()]).unwrap();

        let removed = state.with_removed(&existing).unwrap();
        assert!(removed.resources().next().is_none());

        assert!(matches!(
            state.with_removed(&known("base/git", "store/git/other", "home/.gitconfig")),
            Err(KnownStateError::ResourceMismatch { .. })
        ));
        assert!(matches!(
            state.with_removed(&known("base/zsh", "store/zshrc", "home/.zshrc")),
            Err(KnownStateError::MissingResource { .. })
        ));
        assert!(matches!(
            state.with_missing_resource_removed(
                &FullyQualifiedResourceId::parse("base/zsh").unwrap()
            ),
            Err(KnownStateError::MissingResource { .. })
        ));
    }

    #[test]
    fn identity_handoff_requires_an_absent_destination_identity() {
        let old = known("base/git", "store/git/config", "home/.gitconfig");
        let destination = known("base/git-renamed", "store/git/config", "home/.gitconfig");
        let state = KnownState::new([old.clone(), destination.clone()]).unwrap_err();
        assert!(matches!(state, KnownStateError::DuplicateTarget { .. }));

        let destination = known("base/git-renamed", "store/git/other", "home/.other");
        let state = KnownState::new([old.clone(), destination.clone()]).unwrap();
        assert_eq!(
            state.with_replaced_identity(&old, destination).unwrap_err(),
            KnownStateError::DestinationResourcePresent {
                resource_id: FullyQualifiedResourceId::parse("base/git-renamed").unwrap()
            }
        );
    }

    fn resource_id(id: &str) -> FullyQualifiedResourceId {
        FullyQualifiedResourceId::parse(id).unwrap()
    }

    fn edge(from: &str, to: &str) -> AppliedDirectEdge {
        AppliedDirectEdge {
            from: resource_id(from),
            to: resource_id(to),
            origin: "depends_on",
        }
    }

    fn barrier(id: &str, after: &[&str], members: &[&str]) -> AppliedBarrier {
        AppliedBarrier {
            id: id.into(),
            after: after.iter().map(|item| (*item).into()).collect(),
            members: members.iter().map(|item| resource_id(item)).collect(),
            origin: "stage",
        }
    }

    #[test]
    fn topology_transition_applies_only_exact_authorized_facts_immutably() {
        let direct = edge("root/alpha", "root/beta");
        let early = barrier("stage/root/early", &[], &["root/alpha"]);
        let barrier = barrier(
            "stage/root/late",
            &["stage/root/early"],
            &["root/alpha", "root/beta"],
        );
        let topology = AppliedLifecycleTopology::default();
        let authority = TopologyAuthority {
            current_direct: vec![direct.clone()],
            current_barriers: vec![early.clone(), barrier.clone()],
            ..Default::default()
        };
        let delta = TopologyDelta {
            add_current_direct: vec![direct.clone()],
            add_current_barriers: vec![early.clone(), barrier.clone()],
            ..Default::default()
        };

        let next = topology.transition(&authority, &delta).unwrap();

        assert_eq!(topology, AppliedLifecycleTopology::default());
        assert_eq!(next.current_direct(), &[direct]);
        assert_eq!(next.current_barriers(), &[early, barrier]);
    }

    #[test]
    fn topology_transition_replaces_only_the_exact_shrinking_barrier_projection() {
        let early = barrier("stage/root/early", &[], &["root/alpha"]);
        let before = barrier(
            "stage/root/late",
            &["stage/root/early"],
            &["root/alpha", "root/beta"],
        );
        let after = barrier("stage/root/late", &["stage/root/early"], &["root/beta"]);
        let topology = AppliedLifecycleTopology::try_new(
            vec![],
            vec![],
            vec![early.clone(), before.clone()],
            vec![],
        )
        .unwrap();
        let authority = TopologyAuthority {
            current_barriers: vec![before.clone()],
            ..Default::default()
        };
        let delta = TopologyDelta {
            replace_current_barriers: vec![
                TopologyBarrierReplacement::new(before.clone(), after.clone()).unwrap(),
            ],
            ..Default::default()
        };

        let next = topology.transition(&authority, &delta).unwrap();

        assert_eq!(topology.current_barriers(), &[early.clone(), before]);
        assert_eq!(next.current_barriers(), &[early, after]);
    }

    #[test]
    fn topology_transition_rejects_noncanonical_cross_array_and_unauthorized_changes() {
        let alpha_beta = edge("root/alpha", "root/beta");
        let beta_gamma = edge("root/beta", "root/gamma");
        assert_eq!(
            AppliedLifecycleTopology::try_new(
                vec![beta_gamma.clone(), alpha_beta.clone()],
                vec![],
                vec![],
                vec![],
            )
            .unwrap_err(),
            TopologyTransitionError::NonCanonicalDirectFacts
        );
        assert_eq!(
            AppliedLifecycleTopology::try_new(
                vec![alpha_beta.clone()],
                vec![alpha_beta.clone()],
                vec![],
                vec![],
            )
            .unwrap_err(),
            TopologyTransitionError::CrossArrayDirectFact
        );

        let error = AppliedLifecycleTopology::default()
            .transition(
                &TopologyAuthority::default(),
                &TopologyDelta {
                    add_current_direct: vec![alpha_beta],
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(error, TopologyTransitionError::UnauthorizedFact);
    }

    #[test]
    fn topology_transition_rejects_overlaps_and_mismatched_replacements_without_mutation() {
        let direct = edge("root/alpha", "root/beta");
        let topology =
            AppliedLifecycleTopology::try_new(vec![direct.clone()], vec![], vec![], vec![])
                .unwrap();
        let authority = TopologyAuthority {
            current_direct: vec![direct.clone()],
            ..Default::default()
        };
        let overlapping = TopologyDelta {
            add_current_direct: vec![direct.clone()],
            remove_current_direct: vec![direct.clone()],
            ..Default::default()
        };
        assert_eq!(
            topology.transition(&authority, &overlapping).unwrap_err(),
            TopologyTransitionError::OverlappingDeltaFact
        );

        let before = barrier("stage/root/late", &[], &["root/alpha"]);
        let changed_id = barrier("stage/root/other", &[], &["root/alpha"]);
        assert_eq!(
            TopologyBarrierReplacement::new(before, changed_id).unwrap_err(),
            TopologyTransitionError::ReplacementChangesBarrierId
        );
        assert_eq!(topology.current_direct(), &[direct]);
    }

    #[test]
    fn topology_transition_rejects_replacement_that_changes_barrier_provenance() {
        let before = barrier(
            "stage/root/late",
            &["stage/root/early"],
            &["root/alpha", "root/beta"],
        );
        let changed_after = barrier("stage/root/late", &[], &["root/beta"]);
        assert_eq!(
            TopologyBarrierReplacement::new(before.clone(), changed_after).unwrap_err(),
            TopologyTransitionError::ReplacementChangesBarrierAfter
        );

        let mut changed_origin = barrier("stage/root/late", &["stage/root/early"], &["root/beta"]);
        changed_origin.origin = "other";
        assert_eq!(
            TopologyBarrierReplacement::new(before, changed_origin).unwrap_err(),
            TopologyTransitionError::ReplacementChangesBarrierOrigin
        );
    }

    #[test]
    fn topology_transition_rejects_replacement_without_a_strict_membership_shrink() {
        let before = barrier(
            "stage/root/late",
            &["stage/root/early"],
            &["root/alpha", "root/beta"],
        );
        assert_eq!(
            TopologyBarrierReplacement::new(before.clone(), before).unwrap_err(),
            TopologyTransitionError::ReplacementDoesNotShrinkMembers
        );
    }

    #[test]
    fn topology_transition_rejects_noncanonical_stage_barrier_ids() {
        for id in [
            "stage/late",
            "stage/root/",
            "stage/root//late",
            "stage/root/Late",
            "stage/root/late/extra!",
        ] {
            assert_eq!(
                AppliedLifecycleTopology::try_new(
                    vec![],
                    vec![],
                    vec![barrier(id, &[], &["root/alpha"])],
                    vec![],
                )
                .unwrap_err(),
                TopologyTransitionError::InvalidBarrierId,
                "{id}"
            );
        }

        assert_eq!(
            AppliedLifecycleTopology::try_new(
                vec![],
                vec![],
                vec![barrier(
                    "stage/root/late",
                    &["stage/root//early"],
                    &["root/alpha"],
                )],
                vec![],
            )
            .unwrap_err(),
            TopologyTransitionError::InvalidBarrierAfterId
        );
    }
}
