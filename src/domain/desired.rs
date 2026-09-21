//! Canonical Desired state produced after profile resolution and validation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::domain::file_copy::ResolvedFileCopy;
use crate::domain::file_link::ResolvedFileLink;
use crate::domain::ids::{FullyQualifiedResourceId, ProfileId};
use crate::domain::paths::ResolvedPath;

/// The complete, canonical resource set selected by one root profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedDesired {
    root_profile: ProfileId,
    resources: Vec<ResolvedFileLink>,
    variants: Vec<ResolvedResource>,
}

/// A closed resolved resource effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedResource {
    FileLink(ResolvedFileLink),
    FileCopy(ResolvedFileCopy),
}

impl ResolvedResource {
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

impl From<ResolvedFileLink> for ResolvedResource {
    fn from(resource: ResolvedFileLink) -> Self {
        Self::FileLink(resource)
    }
}

impl From<ResolvedFileCopy> for ResolvedResource {
    fn from(resource: ResolvedFileCopy) -> Self {
        Self::FileCopy(resource)
    }
}

impl ResolvedDesired {
    /// Builds a deterministically ordered Desired set with unique IDs and targets.
    pub(crate) fn new(
        root_profile: ProfileId,
        resources: impl IntoIterator<Item = impl Into<ResolvedResource>>,
    ) -> Result<Self, ResolvedDesiredError> {
        let mut resource_ids = BTreeSet::new();
        let mut targets = BTreeMap::new();
        let mut ordered_variants = Vec::new();

        for resource in resources.into_iter().map(Into::into) {
            let resource_id = resource.resource_id().clone();
            if !resource_ids.insert(resource_id.clone()) {
                return Err(ResolvedDesiredError::DuplicateResourceId { resource_id });
            }

            let target_path = resource.target_path().clone();
            if let Some(first_resource_id) =
                targets.insert(target_path.clone(), resource_id.clone())
            {
                return Err(ResolvedDesiredError::DuplicateTarget {
                    target_path,
                    first_resource_id,
                    duplicate_resource_id: resource_id,
                });
            }

            ordered_variants.push(resource);
        }

        ordered_variants.sort_by_key(|resource| resource.resource_id().clone());
        let resources = ordered_variants
            .iter()
            .filter_map(|resource| match resource {
                ResolvedResource::FileLink(resource) => Some(resource.clone()),
                ResolvedResource::FileCopy(_) => None,
            })
            .collect();

        Ok(Self {
            root_profile,
            resources,
            variants: ordered_variants,
        })
    }

    /// The profile whose include composition produced this Desired set.
    pub(crate) fn root_profile(&self) -> &ProfileId {
        &self.root_profile
    }

    /// Resources sorted by fully qualified resource ID.
    pub(crate) fn resources(&self) -> &[ResolvedFileLink] {
        &self.resources
    }

    /// Every effect variant, sorted by fully qualified resource ID.
    pub(crate) fn variants(&self) -> &[ResolvedResource] {
        &self.variants
    }

    /// Finds one resource by its stable identity.
    pub(crate) fn get(&self, resource_id: &FullyQualifiedResourceId) -> Option<&ResolvedFileLink> {
        self.resources
            .binary_search_by(|resource| resource.resource_id().cmp(resource_id))
            .ok()
            .map(|index| &self.resources[index])
    }

    /// Constructs intentionally invalid Desired input for planner defense tests.
    ///
    /// Runtime resolution must use [`Self::new`], which rejects target collisions before the planner boundary.
    #[cfg(test)]
    pub(crate) fn new_unchecked_for_test(
        root_profile: ProfileId,
        mut resources: Vec<ResolvedFileLink>,
    ) -> Self {
        resources.sort_by_key(|resource| resource.resource_id().clone());

        Self {
            root_profile,
            variants: resources.iter().cloned().map(Into::into).collect(),
            resources,
        }
    }
}

/// The reason a Desired resource set cannot be canonical.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedDesiredError {
    DuplicateResourceId {
        resource_id: FullyQualifiedResourceId,
    },
    DuplicateTarget {
        target_path: ResolvedPath,
        first_resource_id: FullyQualifiedResourceId,
        duplicate_resource_id: FullyQualifiedResourceId,
    },
}

impl fmt::Display for ResolvedDesiredError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateResourceId { resource_id } => {
                write!(formatter, "duplicate desired resource ID: {resource_id}")
            }
            Self::DuplicateTarget {
                target_path,
                first_resource_id,
                duplicate_resource_id,
            } => write!(
                formatter,
                "desired target {target_path} is claimed by both {first_resource_id} and {duplicate_resource_id}"
            ),
        }
    }
}

impl std::error::Error for ResolvedDesiredError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::FullyQualifiedResourceId;

    fn resource(id: &str, source: &str, target: &str) -> ResolvedFileLink {
        let base = std::env::temp_dir().join("loadout-domain-desired");
        ResolvedFileLink::new(
            FullyQualifiedResourceId::parse(id).unwrap(),
            ResolvedPath::new(base.join("store").join(source)).unwrap(),
            ResolvedPath::new(base.join("home").join(target)).unwrap(),
        )
        .unwrap()
    }

    fn root_profile() -> ProfileId {
        ProfileId::parse("workstation").unwrap()
    }

    #[test]
    fn desired_resources_are_sorted_by_fully_qualified_resource_id() {
        let desired = ResolvedDesired::new(
            root_profile(),
            [
                resource("zeta/zsh", "zshrc", ".zshrc"),
                resource("base/git", "git/config", ".gitconfig"),
            ],
        )
        .unwrap();

        assert_eq!(desired.root_profile().as_str(), "workstation");
        assert_eq!(
            desired
                .resources()
                .iter()
                .map(|resource| resource.resource_id().as_str())
                .collect::<Vec<_>>(),
            ["base/git", "zeta/zsh"]
        );
        assert!(
            desired
                .get(&FullyQualifiedResourceId::parse("base/git").unwrap())
                .is_some()
        );
    }

    #[test]
    fn desired_rejects_duplicate_resource_id_and_normalized_target() {
        let duplicate_id = ResolvedDesired::new(
            root_profile(),
            [
                resource("base/git", "git/config", ".gitconfig"),
                resource("base/git", "git/other", ".gitconfig-other"),
            ],
        )
        .unwrap_err();
        assert!(matches!(
            duplicate_id,
            ResolvedDesiredError::DuplicateResourceId { .. }
        ));

        let duplicate_target = ResolvedDesired::new(
            root_profile(),
            [
                resource("base/git", "git/config", ".gitconfig"),
                resource("base/zsh", "zshrc", ".gitconfig"),
            ],
        )
        .unwrap_err();
        assert!(matches!(
            duplicate_target,
            ResolvedDesiredError::DuplicateTarget { .. }
        ));
    }
}
