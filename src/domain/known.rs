//! Verified historical file-link facts owned by the state repository.

use std::collections::BTreeMap;
use std::fmt;

use crate::domain::file_link::{LinkTarget, ResolvedFileLink};
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::paths::ResolvedPath;

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

/// All verified historical resource facts, keyed by stable resource identity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct KnownState {
    resources: BTreeMap<FullyQualifiedResourceId, KnownFileLink>,
}

impl KnownState {
    /// Produces empty state for a machine with no successfully applied resources.
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// Builds state while validating resource-ID and target uniqueness.
    pub(crate) fn new(
        resources: impl IntoIterator<Item = KnownFileLink>,
    ) -> Result<Self, KnownStateError> {
        let mut known = Self::empty();
        let mut targets = BTreeMap::new();

        for resource in resources {
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
        self.resources.get(resource_id)
    }

    /// Iterates over Known resources by fully qualified resource ID.
    pub(crate) fn resources(&self) -> impl ExactSizeIterator<Item = &KnownFileLink> {
        self.resources.values()
    }

    /// Returns a new Known state with one verified resource fact inserted or updated.
    ///
    /// The state repository uses this only in the same atomic commit that records the corresponding action as succeeded.
    pub(crate) fn with_upserted(&self, resource: KnownFileLink) -> Result<Self, KnownStateError> {
        let mut resources = self.resources.clone();
        resources.insert(resource.resource_id().clone(), resource);
        Self::new(resources.into_values())
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
        if actual != expected {
            return Err(KnownStateError::ResourceMismatch {
                resource_id: expected.resource_id().clone(),
            });
        }

        let mut resources = self.resources.clone();
        resources.remove(expected.resource_id());
        Self::new(resources.into_values())
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
        Self::new(resources.into_values())
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
}
