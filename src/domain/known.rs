//! Verified historical file-link facts owned by the state repository.

use std::collections::BTreeMap;
use std::fmt;

use crate::domain::file_copy::{ContentFingerprint, ResolvedFileCopy};
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
}

impl KnownResource {
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        match self {
            Self::FileLink(resource) => resource.resource_id(),
            Self::FileCopy(resource) => resource.resource_id(),
        }
    }
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        match self {
            Self::FileLink(resource) => resource.target_path(),
            Self::FileCopy(resource) => resource.target_path(),
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

/// All verified historical resource facts, keyed by stable resource identity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct KnownState {
    resources: BTreeMap<FullyQualifiedResourceId, KnownResource>,
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

    /// Iterates over file-link facts for the legacy link-only implementation slice.
    pub(crate) fn file_links(&self) -> impl Iterator<Item = &KnownFileLink> {
        self.resources
            .values()
            .filter_map(|resource| match resource {
                KnownResource::FileLink(resource) => Some(resource),
                KnownResource::FileCopy(_) => None,
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
        Self::new(resources.into_values())
    }

    /// Returns a new Known state with one verified copy fact inserted or updated.
    pub(crate) fn with_upserted_copy(
        &self,
        resource: KnownFileCopy,
    ) -> Result<Self, KnownStateError> {
        let mut resources = self.resources.clone();
        resources.insert(resource.resource_id().clone(), resource.into());
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
        if actual != &KnownResource::FileLink(expected.clone()) {
            return Err(KnownStateError::ResourceMismatch {
                resource_id: expected.resource_id().clone(),
            });
        }

        let mut resources = self.resources.clone();
        resources.remove(expected.resource_id());
        Self::new(resources.into_values())
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
}
