//! Deterministic v0.2 action ordering without filesystem or state access.

use crate::domain::plan::{ActionKind, PlannedAction};

/// The fixed v0.2 execution phases.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ActionPhase {
    /// Noop actions are reports only and have no executor phase.
    Noop,
    Create,
    ReplaceOrRelocate,
    RemoveOrForget,
}

impl ActionPhase {
    /// Maps each planned action to its fixed execution phase.
    pub(crate) fn for_action(action: &PlannedAction) -> Self {
        match action.kind() {
            ActionKind::Noop => Self::Noop,
            ActionKind::CreateLink => Self::Create,
            ActionKind::ReplaceLink | ActionKind::ReplaceOwnership | ActionKind::RelocateLink => {
                Self::ReplaceOrRelocate
            }
            ActionKind::RemoveLink | ActionKind::ForgetMissing => Self::RemoveOrForget,
        }
    }
}

/// The deterministic sort key for one action.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ActionSortKey {
    phase: ActionPhase,
    resource_identity: String,
}

impl ActionSortKey {
    /// Creates the phase plus identity key required by the lifecycle specification.
    pub(crate) fn for_action(action: &PlannedAction) -> Self {
        let resource_identity = match action.replaced_resource_id() {
            Some(old_resource_id) => {
                format!("{old_resource_id}\0{}", action.resource_id())
            }
            None => action.resource_id().as_str().to_owned(),
        };

        Self {
            phase: ActionPhase::for_action(action),
            resource_identity,
        }
    }

    /// The fixed phase portion of the ordering key.
    pub(crate) fn phase(&self) -> ActionPhase {
        self.phase
    }

    /// The fully qualified resource-ID portion of the ordering key.
    pub(crate) fn resource_identity(&self) -> &str {
        &self.resource_identity
    }
}

pub(crate) fn sort_actions(actions: &mut [PlannedAction]) {
    actions.sort_by_key(ActionSortKey::for_action);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::file_link::ResolvedFileLink;
    use crate::domain::ids::FullyQualifiedResourceId;
    use crate::domain::known::KnownFileLink;
    use crate::domain::paths::ResolvedPath;

    fn path(name: &str) -> ResolvedPath {
        ResolvedPath::new(
            std::env::temp_dir()
                .join("loadout-planner-ordering")
                .join(name),
        )
        .unwrap()
    }

    fn desired(id: &str, source: &str, target: &str) -> ResolvedFileLink {
        ResolvedFileLink::new(
            FullyQualifiedResourceId::parse(id).unwrap(),
            path(source),
            path(target),
        )
        .unwrap()
    }

    #[test]
    fn ordering_uses_phase_then_fully_qualified_identity() {
        let create = PlannedAction::create_link(desired("zeta/create", "store/z", "home/.z"));
        let replace = PlannedAction::replace_link(
            desired("base/replace", "store/new", "home/.replace"),
            KnownFileLink::from_resolved(&desired("base/replace", "store/old", "home/.replace")),
        )
        .unwrap();
        let remove = PlannedAction::remove_link(KnownFileLink::from_resolved(&desired(
            "alpha/remove",
            "store/remove",
            "home/.remove",
        )));
        let mut actions = vec![remove, replace, create];

        sort_actions(&mut actions);

        assert_eq!(
            actions
                .iter()
                .map(|action| (
                    ActionPhase::for_action(action),
                    action.resource_id().as_str()
                ))
                .collect::<Vec<_>>(),
            [
                (ActionPhase::Create, "zeta/create"),
                (ActionPhase::ReplaceOrRelocate, "base/replace"),
                (ActionPhase::RemoveOrForget, "alpha/remove"),
            ]
        );
    }

    #[test]
    fn ownership_handoff_uses_old_then_new_identity_as_its_tie_breaker() {
        let first = PlannedAction::replace_ownership(
            desired("new/zeta", "store/one", "home/.one"),
            KnownFileLink::from_resolved(&desired("old/alpha", "store/old-one", "home/.one")),
        )
        .unwrap();
        let second = PlannedAction::replace_ownership(
            desired("new/alpha", "store/two", "home/.two"),
            KnownFileLink::from_resolved(&desired("old/zeta", "store/old-two", "home/.two")),
        )
        .unwrap();
        let mut actions = vec![second, first];

        sort_actions(&mut actions);

        let keys = actions
            .iter()
            .map(ActionSortKey::for_action)
            .collect::<Vec<_>>();
        assert_eq!(keys[0].phase(), ActionPhase::ReplaceOrRelocate);
        assert_eq!(keys[0].resource_identity(), "old/alpha\0new/zeta");
        assert_eq!(keys[1].resource_identity(), "old/zeta\0new/alpha");
    }
}
