//! Immediate safety rechecks and execution for planned file-copy creation.

use std::fmt;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::domain::actual::{CopyTargetObservation, TargetObservation};
use crate::domain::desired::ResolvedResource;
use crate::domain::file_copy::ContentFingerprint;
use crate::domain::plan::{
    ActionKind, PlannedEffectHandoff, PlannedFileCopyAction, PlannedResourceAction, TargetCondition,
};
use crate::filesystem::ExecutionTarget;
use crate::inspection::file_link::{FileLinkInspector, TargetInspectionError};
use crate::inspection::source::{SourceVerificationError, VerifiedSource};
use crate::state::operation::RecordedAction;

/// Executes a recorded `create_copy` action without choosing a temporary name or updating state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileCopyExecutor {
    inspector: FileLinkInspector,
}

impl FileCopyExecutor {
    /// Creates an executor rooted at the user's declared and canonical home paths.
    pub(crate) fn new(home_directory: &Path) -> Result<Self, TargetInspectionError> {
        Ok(Self {
            inspector: FileLinkInspector::new(home_directory)?,
        })
    }

    /// Performs the non-mutating source, target, and platform checks required before recording a copy action.
    pub(crate) fn preflight(
        &self,
        action: &PlannedResourceAction,
        source: Option<&VerifiedSource>,
    ) -> Result<(), CopyPreflightError> {
        let expected_source = match action {
            PlannedResourceAction::FileCopy(action) => action.desired().map(|desired| {
                (
                    desired.source_path(),
                    Some(desired.source_content_fingerprint()),
                )
            }),
            PlannedResourceAction::ReplaceEffect(action) => match action.final_effect() {
                ResolvedResource::FileCopy(desired) => Some((
                    desired.source_path(),
                    Some(desired.source_content_fingerprint()),
                )),
                ResolvedResource::FileLink(desired) => Some((desired.source_path(), None)),
            },
            PlannedResourceAction::FileLink(_) => return Err(CopyPreflightError::WrongAction),
        };
        if let Some((expected_path, expected_fingerprint)) = expected_source {
            let source = source.ok_or(CopyPreflightError::MissingVerifiedSource)?;
            let reverified = source
                .reverify()
                .map_err(CopyPreflightError::SourceRecheck)?;
            if reverified.path() != expected_path {
                return Err(CopyPreflightError::SourceDoesNotMatchAction);
            }
            if let Some(expected_fingerprint) = expected_fingerprint {
                if fingerprint_regular_file(reverified.path())? != *expected_fingerprint {
                    return Err(CopyPreflightError::SourceFingerprintChanged);
                }
            }
        }
        for condition in action.preconditions() {
            self.preflight_condition(&condition)?;
        }
        self.ensure_platform_capability(action)
    }

    fn preflight_condition(&self, condition: &TargetCondition) -> Result<(), CopyPreflightError> {
        match condition {
            TargetCondition::Missing { target_path } => {
                let observation = self
                    .inspector
                    .inspect_target_for_expected_copy(
                        target_path,
                        &ContentFingerprint::parse(format!("sha256:{}", "0".repeat(64)))
                            .expect("fixed fingerprint is canonical"),
                    )
                    .map_err(CopyPreflightError::TargetInspection)?;
                if matches!(observation.observation(), CopyTargetObservation::Missing) {
                    Ok(())
                } else {
                    Err(CopyPreflightError::PreconditionNoLongerHolds)
                }
            }
            TargetCondition::ExpectedCopy {
                target_path,
                content_fingerprint,
            } => {
                let observation = self
                    .inspector
                    .inspect_target_for_expected_copy(target_path, content_fingerprint)
                    .map_err(CopyPreflightError::TargetInspection)?;
                if matches!(
                    observation.observation(),
                    CopyTargetObservation::ExpectedCopy { .. }
                ) {
                    Ok(())
                } else {
                    Err(CopyPreflightError::PreconditionNoLongerHolds)
                }
            }
            TargetCondition::ExpectedLink {
                target_path,
                link_target,
            } => {
                let observation = self
                    .inspector
                    .inspect_target_for_expected_link(target_path, link_target)
                    .map_err(CopyPreflightError::TargetInspection)?;
                if matches!(
                    observation.observation(),
                    TargetObservation::ExpectedLink { .. }
                ) {
                    Ok(())
                } else {
                    Err(CopyPreflightError::PreconditionNoLongerHolds)
                }
            }
        }
    }

    fn ensure_platform_capability(
        &self,
        action: &PlannedResourceAction,
    ) -> Result<(), CopyPreflightError> {
        match action {
            PlannedResourceAction::FileCopy(action)
                if matches!(action.kind(), ActionKind::Noop | ActionKind::ForgetMissing) =>
            {
                Ok(())
            }
            PlannedResourceAction::FileCopy(_) => self.ensure_copy_mutation_capability(action),
            PlannedResourceAction::ReplaceEffect(handoff) => match handoff.final_effect() {
                ResolvedResource::FileCopy(_) => self.ensure_copy_mutation_capability(action),
                ResolvedResource::FileLink(_) => self.ensure_link_mutation_capability(action),
            },
            PlannedResourceAction::FileLink(_) => Err(CopyPreflightError::WrongAction),
        }
    }

    fn ensure_copy_mutation_capability(
        &self,
        action: &PlannedResourceAction,
    ) -> Result<(), CopyPreflightError> {
        self.ensure_mutation_capability(action)
    }

    fn ensure_link_mutation_capability(
        &self,
        action: &PlannedResourceAction,
    ) -> Result<(), CopyPreflightError> {
        self.ensure_mutation_capability(action)
    }

    fn ensure_mutation_capability(
        &self,
        action: &PlannedResourceAction,
    ) -> Result<(), CopyPreflightError> {
        #[cfg(target_os = "linux")]
        {
            const EXT_SUPER_MAGIC: i64 = 0xEF53;
            let supported_filesystems = action.touched_targets().into_iter().all(|target| {
                target.as_ref().parent().is_some_and(|parent| {
                    rustix::fs::statfs(parent)
                        .is_ok_and(|filesystem| filesystem.f_type == EXT_SUPER_MAGIC)
                })
            });
            // An empty old name cannot name an entry, so a NotFound result proves that the
            // kernel accepted renameat2 and RENAME_NOREPLACE without mutating a target.
            let rename_no_replace_supported = rustix::fs::renameat_with(
                rustix::fs::CWD,
                "",
                rustix::fs::CWD,
                "",
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .is_err_and(|error| io::Error::from(error).kind() == io::ErrorKind::NotFound);
            if supported_filesystems && rename_no_replace_supported {
                return Ok(());
            }
        }
        let _ = action;
        Err(CopyPreflightError::UnsupportedPlatformCapability)
    }

    /// Materializes one planned and recorded copy only after every immediate recheck succeeds.
    ///
    /// The caller must durably mark the action `running` before this method and atomically commit its Known update only after this method returns success.
    #[allow(dead_code)] // M3-B connects this executor to the operation coordinator after all copy action records exist.
    pub(crate) fn execute_create(
        &self,
        action: &PlannedFileCopyAction,
        recorded: &RecordedAction,
        source: &VerifiedSource,
    ) -> Result<(), CreateCopyExecutionError> {
        if action.kind() != ActionKind::CreateCopy || recorded.kind() != ActionKind::CreateCopy {
            return Err(CreateCopyExecutionError::UnsupportedAction {
                action_kind: action.kind(),
                recorded_kind: recorded.kind(),
            });
        }
        let desired = action
            .desired()
            .ok_or(CreateCopyExecutionError::MissingDesired)?;
        let facts = recorded
            .copy_facts()
            .ok_or(CreateCopyExecutionError::MissingRecordedFacts)?;
        if action.resource_id() != recorded.resource_id()
            || facts.source_path() != desired.source_path()
            || facts.target_path() != desired.target_path()
            || facts.content_fingerprint() != desired.source_content_fingerprint()
        {
            return Err(CreateCopyExecutionError::RecordDoesNotMatchAction);
        }
        if facts.temporary_path().as_ref().parent() != facts.target_path().as_ref().parent()
            || facts.temporary_path() == facts.target_path()
        {
            return Err(CreateCopyExecutionError::InvalidTemporaryPath);
        }

        let reverified = source
            .reverify()
            .map_err(CreateCopyExecutionError::SourceRecheck)?;
        if reverified.path() != facts.source_path() {
            return Err(CreateCopyExecutionError::SourceDoesNotMatchAction {
                expected: facts.source_path().clone(),
                actual: reverified.path().clone(),
            });
        }
        let before = self
            .inspector
            .inspect_target_for_expected_copy(facts.target_path(), facts.content_fingerprint())
            .map_err(CreateCopyExecutionError::TargetInspection)?;
        if !matches!(before.observation(), CopyTargetObservation::Missing) {
            return Err(CreateCopyExecutionError::PreconditionNoLongerHolds {
                observation: before.observation().clone(),
            });
        }
        let target = self
            .inspector
            .physical_target_path_for_execution(facts.target_path())
            .map_err(CreateCopyExecutionError::TargetInspection)?;
        let temporary = self
            .inspector
            .physical_target_path_for_execution(facts.temporary_path())
            .map_err(CreateCopyExecutionError::TemporaryInspection)?;
        let target_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &target,
        )
        .map_err(CreateCopyExecutionError::Filesystem)?;
        let temporary_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &temporary,
        )
        .map_err(CreateCopyExecutionError::Filesystem)?;

        temporary_context
            .copy_source_to_temporary(
                reverified.physical_root(),
                reverified.path(),
                facts.content_fingerprint(),
            )
            .map_err(CreateCopyExecutionError::Filesystem)?;
        target_context
            .publish_copy_no_replace(&temporary, facts.content_fingerprint())
            .map_err(CreateCopyExecutionError::Filesystem)?;

        let final_observation = self
            .inspector
            .inspect_target_for_expected_copy(facts.target_path(), facts.content_fingerprint())
            .map_err(CreateCopyExecutionError::PostconditionInspection)?;
        let temporary_observation = self
            .inspector
            .inspect_target_for_expected_copy(facts.temporary_path(), facts.content_fingerprint())
            .map_err(CreateCopyExecutionError::TemporaryInspection)?;
        if matches!(
            final_observation.observation(),
            CopyTargetObservation::ExpectedCopy { .. }
        ) && matches!(
            temporary_observation.observation(),
            CopyTargetObservation::Missing
        ) {
            Ok(())
        } else {
            Err(CreateCopyExecutionError::PostconditionNotMet {
                final_observation: final_observation.observation().clone(),
                temporary_observation: temporary_observation.observation().clone(),
            })
        }
    }

    /// Replaces one rechecked owned copy through its exact recorded temporary sibling.
    #[allow(dead_code)] // M3-B connects this executor to the operation coordinator after all copy action records exist.
    pub(crate) fn execute_replace(
        &self,
        action: &PlannedFileCopyAction,
        recorded: &RecordedAction,
        source: &VerifiedSource,
    ) -> Result<(), ReplaceCopyExecutionError> {
        if action.kind() != ActionKind::ReplaceCopy || recorded.kind() != ActionKind::ReplaceCopy {
            return Err(ReplaceCopyExecutionError::UnsupportedAction);
        }
        let desired = action
            .desired()
            .ok_or(ReplaceCopyExecutionError::MissingFacts)?;
        let previous = action
            .previous()
            .ok_or(ReplaceCopyExecutionError::MissingFacts)?;
        let facts = recorded
            .copy_facts()
            .ok_or(ReplaceCopyExecutionError::MissingFacts)?;
        if action.resource_id() != recorded.resource_id()
            || facts.source_path() != desired.source_path()
            || facts.target_path() != desired.target_path()
            || facts.content_fingerprint() != desired.source_content_fingerprint()
            || facts.old_effect()
                != Some(&crate::domain::known::KnownResource::FileCopy(
                    previous.clone(),
                ))
            || facts.temporary_path().as_ref().parent() != facts.target_path().as_ref().parent()
            || facts.temporary_path() == facts.target_path()
        {
            return Err(ReplaceCopyExecutionError::RecordDoesNotMatchAction);
        }
        let reverified = source
            .reverify()
            .map_err(ReplaceCopyExecutionError::SourceRecheck)?;
        if reverified.path() != facts.source_path() {
            return Err(ReplaceCopyExecutionError::SourceDoesNotMatchAction);
        }
        let before = self
            .inspector
            .inspect_target_for_expected_copy(
                previous.target_path(),
                previous.content_fingerprint(),
            )
            .map_err(ReplaceCopyExecutionError::TargetInspection)?;
        if !matches!(
            before.observation(),
            CopyTargetObservation::ExpectedCopy { .. }
        ) {
            return Err(ReplaceCopyExecutionError::PreconditionNoLongerHolds {
                observation: before.observation().clone(),
            });
        }
        let target = self
            .inspector
            .physical_target_path_for_execution(facts.target_path())
            .map_err(ReplaceCopyExecutionError::TargetInspection)?;
        let temporary = self
            .inspector
            .physical_target_path_for_execution(facts.temporary_path())
            .map_err(ReplaceCopyExecutionError::TemporaryInspection)?;
        let target_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &target,
        )
        .map_err(ReplaceCopyExecutionError::Filesystem)?;
        let temporary_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &temporary,
        )
        .map_err(ReplaceCopyExecutionError::Filesystem)?;
        temporary_context
            .copy_source_to_temporary(
                reverified.physical_root(),
                reverified.path(),
                facts.content_fingerprint(),
            )
            .map_err(ReplaceCopyExecutionError::Filesystem)?;
        target_context
            .replace_copy_from_temporary(
                &temporary,
                previous.content_fingerprint(),
                facts.content_fingerprint(),
            )
            .map_err(ReplaceCopyExecutionError::Filesystem)?;
        let final_observation = self
            .inspector
            .inspect_target_for_expected_copy(facts.target_path(), facts.content_fingerprint())
            .map_err(ReplaceCopyExecutionError::PostconditionInspection)?;
        let temporary_observation = self
            .inspector
            .inspect_target_for_expected_copy(facts.temporary_path(), facts.content_fingerprint())
            .map_err(ReplaceCopyExecutionError::TemporaryInspection)?;
        if matches!(
            final_observation.observation(),
            CopyTargetObservation::ExpectedCopy { .. }
        ) && matches!(
            temporary_observation.observation(),
            CopyTargetObservation::Missing
        ) {
            Ok(())
        } else {
            Err(ReplaceCopyExecutionError::PostconditionNotMet)
        }
    }

    /// Removes one stale copy only after its current bytes prove the exact Known fingerprint.
    #[allow(dead_code)] // M3-B connects this executor to the operation coordinator after copy removal records exist.
    pub(crate) fn execute_remove(
        &self,
        action: &PlannedFileCopyAction,
    ) -> Result<(), RemoveCopyExecutionError> {
        if action.kind() != ActionKind::RemoveCopy {
            return Err(RemoveCopyExecutionError::UnsupportedAction);
        }
        let previous = action
            .previous()
            .ok_or(RemoveCopyExecutionError::MissingPrevious)?;
        let before = self
            .inspector
            .inspect_target_for_expected_copy(
                previous.target_path(),
                previous.content_fingerprint(),
            )
            .map_err(RemoveCopyExecutionError::TargetInspection)?;
        if !matches!(
            before.observation(),
            CopyTargetObservation::ExpectedCopy { .. }
        ) {
            return Err(RemoveCopyExecutionError::PreconditionNoLongerHolds {
                observation: before.observation().clone(),
            });
        }
        let target = self
            .inspector
            .physical_target_path_for_execution(previous.target_path())
            .map_err(RemoveCopyExecutionError::TargetInspection)?;
        let context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &target,
        )
        .map_err(RemoveCopyExecutionError::Filesystem)?;
        context
            .remove_expected_copy(previous.content_fingerprint())
            .map_err(RemoveCopyExecutionError::Filesystem)?;
        let after = self
            .inspector
            .inspect_target_for_expected_copy(
                previous.target_path(),
                previous.content_fingerprint(),
            )
            .map_err(RemoveCopyExecutionError::PostconditionInspection)?;
        if matches!(after.observation(), CopyTargetObservation::Missing) {
            Ok(())
        } else {
            Err(RemoveCopyExecutionError::PostconditionNotMet {
                observation: after.observation().clone(),
            })
        }
    }

    /// Relocates a copy by publishing and verifying the new target before removing the old owned copy.
    #[allow(dead_code)] // M3-B connects this executor to the operation coordinator after all copy action records exist.
    pub(crate) fn execute_relocate(
        &self,
        action: &PlannedFileCopyAction,
        recorded: &RecordedAction,
        source: &VerifiedSource,
    ) -> Result<(), RelocateCopyExecutionError> {
        if action.kind() != ActionKind::RelocateCopy || recorded.kind() != ActionKind::RelocateCopy
        {
            return Err(RelocateCopyExecutionError::UnsupportedAction);
        }
        let desired = action
            .desired()
            .ok_or(RelocateCopyExecutionError::MissingFacts)?;
        let previous = action
            .previous()
            .ok_or(RelocateCopyExecutionError::MissingFacts)?;
        let facts = recorded
            .copy_facts()
            .ok_or(RelocateCopyExecutionError::MissingFacts)?;
        if facts.source_path() != desired.source_path()
            || facts.target_path() != desired.target_path()
            || facts.content_fingerprint() != desired.source_content_fingerprint()
            || facts.old_effect()
                != Some(&crate::domain::known::KnownResource::FileCopy(
                    previous.clone(),
                ))
            || facts.target_path() == previous.target_path()
            || facts.temporary_path().as_ref().parent() != facts.target_path().as_ref().parent()
        {
            return Err(RelocateCopyExecutionError::RecordDoesNotMatchAction);
        }
        let source = source
            .reverify()
            .map_err(RelocateCopyExecutionError::SourceRecheck)?;
        if source.path() != facts.source_path() {
            return Err(RelocateCopyExecutionError::SourceDoesNotMatchAction);
        }
        let old_before = self
            .inspector
            .inspect_target_for_expected_copy(
                previous.target_path(),
                previous.content_fingerprint(),
            )
            .map_err(RelocateCopyExecutionError::OldTargetInspection)?;
        let new_before = self
            .inspector
            .inspect_target_for_expected_copy(facts.target_path(), facts.content_fingerprint())
            .map_err(RelocateCopyExecutionError::NewTargetInspection)?;
        if !matches!(
            old_before.observation(),
            CopyTargetObservation::ExpectedCopy { .. }
        ) || !matches!(new_before.observation(), CopyTargetObservation::Missing)
        {
            return Err(RelocateCopyExecutionError::PreconditionNoLongerHolds {
                old_observation: old_before.observation().clone(),
                new_observation: new_before.observation().clone(),
            });
        }
        let old_target = self
            .inspector
            .physical_target_path_for_execution(previous.target_path())
            .map_err(RelocateCopyExecutionError::OldTargetInspection)?;
        let new_target = self
            .inspector
            .physical_target_path_for_execution(facts.target_path())
            .map_err(RelocateCopyExecutionError::NewTargetInspection)?;
        let temporary = self
            .inspector
            .physical_target_path_for_execution(facts.temporary_path())
            .map_err(RelocateCopyExecutionError::TemporaryInspection)?;
        let old_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &old_target,
        )
        .map_err(RelocateCopyExecutionError::Filesystem)?;
        let new_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &new_target,
        )
        .map_err(RelocateCopyExecutionError::Filesystem)?;
        let temporary_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &temporary,
        )
        .map_err(RelocateCopyExecutionError::Filesystem)?;
        temporary_context
            .copy_source_to_temporary(
                source.physical_root(),
                source.path(),
                facts.content_fingerprint(),
            )
            .map_err(RelocateCopyExecutionError::Filesystem)?;
        new_context
            .publish_copy_no_replace(&temporary, facts.content_fingerprint())
            .map_err(RelocateCopyExecutionError::Filesystem)?;
        let new_after_publish = self
            .inspector
            .inspect_target_for_expected_copy(facts.target_path(), facts.content_fingerprint())
            .map_err(RelocateCopyExecutionError::NewTargetInspection)?;
        if !matches!(
            new_after_publish.observation(),
            CopyTargetObservation::ExpectedCopy { .. }
        ) {
            return Err(RelocateCopyExecutionError::NewPostconditionNotMet {
                observation: new_after_publish.observation().clone(),
            });
        }
        old_context
            .remove_expected_copy(previous.content_fingerprint())
            .map_err(RelocateCopyExecutionError::Filesystem)?;
        let old_after = self
            .inspector
            .inspect_target_for_expected_copy(
                previous.target_path(),
                previous.content_fingerprint(),
            )
            .map_err(RelocateCopyExecutionError::OldTargetInspection)?;
        let temporary_after = self
            .inspector
            .inspect_target_for_expected_copy(facts.temporary_path(), facts.content_fingerprint())
            .map_err(RelocateCopyExecutionError::TemporaryInspection)?;
        if matches!(old_after.observation(), CopyTargetObservation::Missing)
            && matches!(
                temporary_after.observation(),
                CopyTargetObservation::Missing
            )
        {
            Ok(())
        } else {
            Err(RelocateCopyExecutionError::PostconditionNotMet {
                old_observation: old_after.observation().clone(),
                temporary_observation: temporary_after.observation().clone(),
            })
        }
    }

    /// Replaces one expected managed link with a verified copy using the recorded handoff temporary.
    #[allow(dead_code)] // M3-B connects this executor to the operation coordinator after handoff actions are recorded.
    pub(crate) fn execute_link_to_copy_handoff(
        &self,
        action: &PlannedEffectHandoff,
        recorded: &RecordedAction,
        source: &VerifiedSource,
    ) -> Result<(), LinkToCopyHandoffExecutionError> {
        let crate::domain::known::KnownResource::FileLink(old_link) = action.old_effect() else {
            return Err(LinkToCopyHandoffExecutionError::InvalidEffectPair);
        };
        let crate::domain::desired::ResolvedResource::FileCopy(final_copy) = action.final_effect()
        else {
            return Err(LinkToCopyHandoffExecutionError::InvalidEffectPair);
        };
        if recorded.kind() != ActionKind::ReplaceEffect
            || action.resource_id() != recorded.resource_id()
        {
            return Err(LinkToCopyHandoffExecutionError::RecordDoesNotMatchAction);
        }
        let facts = recorded
            .effect_handoff_facts()
            .ok_or(LinkToCopyHandoffExecutionError::MissingRecordedFacts)?;
        let final_known = crate::domain::known::KnownFileCopy::from_resolved(final_copy);
        if facts.old_effect() != &crate::domain::known::KnownResource::FileLink(old_link.clone())
            || facts.final_effect() != &crate::domain::known::KnownResource::FileCopy(final_known)
            || facts.temporary_path().as_ref().parent()
                != final_copy.target_path().as_ref().parent()
            || facts.temporary_path() == final_copy.target_path()
        {
            return Err(LinkToCopyHandoffExecutionError::RecordDoesNotMatchAction);
        }
        let source = source
            .reverify()
            .map_err(LinkToCopyHandoffExecutionError::SourceRecheck)?;
        if source.path() != final_copy.source_path() {
            return Err(LinkToCopyHandoffExecutionError::SourceDoesNotMatchAction);
        }
        let before = self
            .inspector
            .inspect_target_for_expected_link(old_link.target_path(), old_link.link_target())
            .map_err(LinkToCopyHandoffExecutionError::TargetInspection)?;
        if !matches!(
            before.observation(),
            crate::domain::actual::TargetObservation::ExpectedLink { .. }
        ) {
            return Err(LinkToCopyHandoffExecutionError::PreconditionNoLongerHolds);
        }
        let target = self
            .inspector
            .physical_target_path_for_execution(final_copy.target_path())
            .map_err(LinkToCopyHandoffExecutionError::TargetInspection)?;
        let temporary = self
            .inspector
            .physical_target_path_for_execution(facts.temporary_path())
            .map_err(LinkToCopyHandoffExecutionError::TemporaryInspection)?;
        let target_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &target,
        )
        .map_err(LinkToCopyHandoffExecutionError::Filesystem)?;
        let temporary_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &temporary,
        )
        .map_err(LinkToCopyHandoffExecutionError::Filesystem)?;
        temporary_context
            .copy_source_to_temporary(
                source.physical_root(),
                source.path(),
                final_copy.source_content_fingerprint(),
            )
            .map_err(LinkToCopyHandoffExecutionError::Filesystem)?;
        target_context
            .replace_link_with_copy_temporary(
                &temporary,
                old_link.link_target(),
                final_copy.source_content_fingerprint(),
            )
            .map_err(LinkToCopyHandoffExecutionError::Filesystem)?;
        let final_observation = self
            .inspector
            .inspect_target_for_expected_copy(
                final_copy.target_path(),
                final_copy.source_content_fingerprint(),
            )
            .map_err(LinkToCopyHandoffExecutionError::PostconditionInspection)?;
        let temporary_observation = self
            .inspector
            .inspect_target_for_expected_copy(
                facts.temporary_path(),
                final_copy.source_content_fingerprint(),
            )
            .map_err(LinkToCopyHandoffExecutionError::TemporaryInspection)?;
        if matches!(
            final_observation.observation(),
            CopyTargetObservation::ExpectedCopy { .. }
        ) && matches!(
            temporary_observation.observation(),
            CopyTargetObservation::Missing
        ) {
            Ok(())
        } else {
            Err(LinkToCopyHandoffExecutionError::PostconditionNotMet)
        }
    }

    /// Replaces one expected managed copy with a verified link using the recorded handoff temporary.
    #[allow(dead_code)] // M3-B connects this executor to the operation coordinator after handoff actions are recorded.
    pub(crate) fn execute_copy_to_link_handoff(
        &self,
        action: &PlannedEffectHandoff,
        recorded: &RecordedAction,
        source: &VerifiedSource,
    ) -> Result<(), CopyToLinkHandoffExecutionError> {
        let crate::domain::known::KnownResource::FileCopy(old_copy) = action.old_effect() else {
            return Err(CopyToLinkHandoffExecutionError::InvalidEffectPair);
        };
        let crate::domain::desired::ResolvedResource::FileLink(final_link) = action.final_effect()
        else {
            return Err(CopyToLinkHandoffExecutionError::InvalidEffectPair);
        };
        if recorded.kind() != ActionKind::ReplaceEffect
            || action.resource_id() != recorded.resource_id()
        {
            return Err(CopyToLinkHandoffExecutionError::RecordDoesNotMatchAction);
        }
        let facts = recorded
            .effect_handoff_facts()
            .ok_or(CopyToLinkHandoffExecutionError::MissingRecordedFacts)?;
        let final_known = crate::domain::known::KnownFileLink::from_resolved(final_link);
        if facts.old_effect() != &crate::domain::known::KnownResource::FileCopy(old_copy.clone())
            || facts.final_effect() != &crate::domain::known::KnownResource::FileLink(final_known)
            || facts.temporary_path().as_ref().parent()
                != final_link.target_path().as_ref().parent()
            || facts.temporary_path() == final_link.target_path()
        {
            return Err(CopyToLinkHandoffExecutionError::RecordDoesNotMatchAction);
        }
        let source = source
            .reverify()
            .map_err(CopyToLinkHandoffExecutionError::SourceRecheck)?;
        if source.path() != final_link.source_path() {
            return Err(CopyToLinkHandoffExecutionError::SourceDoesNotMatchAction);
        }
        let before = self
            .inspector
            .inspect_target_for_expected_copy(
                old_copy.target_path(),
                old_copy.content_fingerprint(),
            )
            .map_err(CopyToLinkHandoffExecutionError::TargetInspection)?;
        if !matches!(
            before.observation(),
            CopyTargetObservation::ExpectedCopy { .. }
        ) {
            return Err(CopyToLinkHandoffExecutionError::PreconditionNoLongerHolds);
        }
        let target = self
            .inspector
            .physical_target_path_for_execution(final_link.target_path())
            .map_err(CopyToLinkHandoffExecutionError::TargetInspection)?;
        let temporary = self
            .inspector
            .physical_target_path_for_execution(facts.temporary_path())
            .map_err(CopyToLinkHandoffExecutionError::TemporaryInspection)?;
        let target_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &target,
        )
        .map_err(CopyToLinkHandoffExecutionError::Filesystem)?;
        let temporary_context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &temporary,
        )
        .map_err(CopyToLinkHandoffExecutionError::Filesystem)?;
        temporary_context
            .create_link_temporary(source.physical_root(), final_link.link_target())
            .map_err(CopyToLinkHandoffExecutionError::Filesystem)?;
        target_context
            .replace_copy_with_link_temporary(
                &temporary,
                old_copy.content_fingerprint(),
                final_link.link_target(),
            )
            .map_err(CopyToLinkHandoffExecutionError::Filesystem)?;
        let final_observation = self
            .inspector
            .inspect_target_for_expected_link(final_link.target_path(), final_link.link_target())
            .map_err(CopyToLinkHandoffExecutionError::PostconditionInspection)?;
        let temporary_observation = self
            .inspector
            .inspect_target_for_expected_link(facts.temporary_path(), final_link.link_target())
            .map_err(CopyToLinkHandoffExecutionError::TemporaryInspection)?;
        if matches!(
            final_observation.observation(),
            crate::domain::actual::TargetObservation::ExpectedLink { .. }
        ) && matches!(
            temporary_observation.observation(),
            crate::domain::actual::TargetObservation::Missing
        ) {
            Ok(())
        } else {
            Err(CopyToLinkHandoffExecutionError::PostconditionNotMet)
        }
    }

    /// Removes only an exact recorded handoff or copy temporary after proving its final effect.
    #[allow(dead_code)] // M3-C wires this cleanup into operation recovery.
    pub(crate) fn cleanup_recorded_temporary(
        &self,
        recorded: &RecordedAction,
    ) -> Result<(), CopyTemporaryCleanupError> {
        let temporary_path = recorded
            .temporary_path()
            .ok_or(CopyTemporaryCleanupError::NoTemporary)?;
        let final_effect = if let Some(facts) = recorded.copy_facts() {
            facts.final_effect().clone()
        } else if let Some(facts) = recorded.effect_handoff_facts() {
            facts.final_effect().clone()
        } else {
            return Err(CopyTemporaryCleanupError::NoTemporary);
        };
        let temporary = self
            .inspector
            .physical_target_path_for_execution(temporary_path)
            .map_err(CopyTemporaryCleanupError::Inspection)?;
        let context = ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &temporary,
        )
        .map_err(CopyTemporaryCleanupError::Filesystem)?;
        match final_effect {
            crate::domain::known::KnownResource::FileCopy(copy) => {
                let observation = self
                    .inspector
                    .inspect_target_for_expected_copy(temporary_path, copy.content_fingerprint())
                    .map_err(CopyTemporaryCleanupError::Inspection)?;
                if matches!(observation.observation(), CopyTargetObservation::Missing) {
                    return Ok(());
                }
                if !matches!(
                    observation.observation(),
                    CopyTargetObservation::ExpectedCopy { .. }
                ) {
                    return Err(CopyTemporaryCleanupError::UnexpectedTemporary);
                }
                context
                    .remove_expected_copy(copy.content_fingerprint())
                    .map_err(CopyTemporaryCleanupError::Filesystem)?;
            }
            crate::domain::known::KnownResource::FileLink(link) => {
                let observation = self
                    .inspector
                    .inspect_target_for_expected_link(temporary_path, link.link_target())
                    .map_err(CopyTemporaryCleanupError::Inspection)?;
                if matches!(
                    observation.observation(),
                    crate::domain::actual::TargetObservation::Missing
                ) {
                    return Ok(());
                }
                if !matches!(
                    observation.observation(),
                    crate::domain::actual::TargetObservation::ExpectedLink { .. }
                ) {
                    return Err(CopyTemporaryCleanupError::UnexpectedTemporary);
                }
                context
                    .prepare_remove(link.link_target())
                    .and_then(|checked| checked.attempt())
                    .map_err(CopyTemporaryCleanupError::Filesystem)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) enum CopyTemporaryCleanupError {
    NoTemporary,
    Inspection(TargetInspectionError),
    Filesystem(io::Error),
    UnexpectedTemporary,
}

impl fmt::Display for CopyTemporaryCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTemporary => formatter.write_str("action has no recorded temporary"),
            Self::Inspection(error) => error.fmt(formatter),
            Self::Filesystem(error) => write!(formatter, "temporary cleanup failed: {error}"),
            Self::UnexpectedTemporary => {
                formatter.write_str("recorded temporary is not the expected owned effect")
            }
        }
    }
}

impl std::error::Error for CopyTemporaryCleanupError {}

fn fingerprint_regular_file(
    path: &crate::domain::paths::ResolvedPath,
) -> Result<ContentFingerprint, CopyPreflightError> {
    let mut file =
        std::fs::File::open(path.as_ref()).map_err(CopyPreflightError::SourceFingerprint)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(CopyPreflightError::SourceFingerprint)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    ContentFingerprint::parse(format!("sha256:{:x}", hasher.finalize()))
        .map_err(|_| CopyPreflightError::SourceFingerprintFormat)
}

/// The reason a copy action cannot pass the pre-operation safety gate.
#[derive(Debug)]
pub(crate) enum CopyPreflightError {
    WrongAction,
    MissingVerifiedSource,
    SourceRecheck(SourceVerificationError),
    SourceDoesNotMatchAction,
    SourceFingerprint(io::Error),
    SourceFingerprintFormat,
    SourceFingerprintChanged,
    TargetInspection(TargetInspectionError),
    PreconditionNoLongerHolds,
    UnsupportedPlatformCapability,
}

impl fmt::Display for CopyPreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongAction => {
                formatter.write_str("file-copy preflight requires a copy or effect-handoff action")
            }
            Self::MissingVerifiedSource => {
                formatter.write_str("copy action is missing its verified source")
            }
            Self::SourceRecheck(error) => error.fmt(formatter),
            Self::SourceDoesNotMatchAction => {
                formatter.write_str("rechecked source does not match the planned copy action")
            }
            Self::SourceFingerprint(error) => write!(
                formatter,
                "cannot fingerprint rechecked copy source: {error}"
            ),
            Self::SourceFingerprintFormat => {
                formatter.write_str("rechecked copy source fingerprint is invalid")
            }
            Self::SourceFingerprintChanged => formatter
                .write_str("rechecked copy source bytes differ from the planned fingerprint"),
            Self::TargetInspection(error) => error.fmt(formatter),
            Self::PreconditionNoLongerHolds => {
                formatter.write_str("copy target precondition no longer holds")
            }
            Self::UnsupportedPlatformCapability => formatter
                .write_str("file-copy publication capability is unsupported on this platform"),
        }
    }
}

impl std::error::Error for CopyPreflightError {}

/// The reason a selected `create_copy` action could not be proven successful.
#[derive(Debug)]
pub(crate) enum CreateCopyExecutionError {
    UnsupportedAction {
        action_kind: ActionKind,
        recorded_kind: ActionKind,
    },
    MissingDesired,
    MissingRecordedFacts,
    RecordDoesNotMatchAction,
    InvalidTemporaryPath,
    SourceRecheck(SourceVerificationError),
    SourceDoesNotMatchAction {
        expected: crate::domain::paths::ResolvedPath,
        actual: crate::domain::paths::ResolvedPath,
    },
    TargetInspection(TargetInspectionError),
    TemporaryInspection(TargetInspectionError),
    Filesystem(io::Error),
    PreconditionNoLongerHolds {
        observation: CopyTargetObservation,
    },
    PostconditionInspection(TargetInspectionError),
    PostconditionNotMet {
        final_observation: CopyTargetObservation,
        temporary_observation: CopyTargetObservation,
    },
}

impl fmt::Display for CreateCopyExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAction {
                action_kind,
                recorded_kind,
            } => write!(
                formatter,
                "copy executor requires matching create_copy action and record, got {action_kind:?} and {recorded_kind:?}"
            ),
            Self::MissingDesired => formatter.write_str("create_copy action has no desired copy"),
            Self::MissingRecordedFacts => {
                formatter.write_str("create_copy record has no copy facts")
            }
            Self::RecordDoesNotMatchAction => {
                formatter.write_str("recorded copy facts do not match the planned action")
            }
            Self::InvalidTemporaryPath => {
                formatter.write_str("recorded copy temporary is not a distinct target sibling")
            }
            Self::SourceRecheck(error) => error.fmt(formatter),
            Self::SourceDoesNotMatchAction { expected, actual } => write!(
                formatter,
                "rechecked source {actual} does not match planned source {expected}"
            ),
            Self::TargetInspection(error)
            | Self::TemporaryInspection(error)
            | Self::PostconditionInspection(error) => error.fmt(formatter),
            Self::Filesystem(error) => {
                write!(formatter, "copy filesystem operation failed: {error}")
            }
            Self::PreconditionNoLongerHolds { observation } => write!(
                formatter,
                "copy target precondition no longer holds: {observation:?}"
            ),
            Self::PostconditionNotMet {
                final_observation,
                temporary_observation,
            } => write!(
                formatter,
                "copy postcondition not met: final={final_observation:?}, temporary={temporary_observation:?}"
            ),
        }
    }
}

impl std::error::Error for CreateCopyExecutionError {}

/// The reason a selected `replace_copy` action could not be proven successful.
#[derive(Debug)]
pub(crate) enum ReplaceCopyExecutionError {
    UnsupportedAction,
    MissingFacts,
    RecordDoesNotMatchAction,
    SourceRecheck(SourceVerificationError),
    SourceDoesNotMatchAction,
    TargetInspection(TargetInspectionError),
    TemporaryInspection(TargetInspectionError),
    Filesystem(io::Error),
    PreconditionNoLongerHolds { observation: CopyTargetObservation },
    PostconditionInspection(TargetInspectionError),
    PostconditionNotMet,
}

impl fmt::Display for ReplaceCopyExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAction => formatter
                .write_str("copy executor requires a matching replace_copy action and record"),
            Self::MissingFacts => {
                formatter.write_str("replace_copy is missing required planned or recorded facts")
            }
            Self::RecordDoesNotMatchAction => {
                formatter.write_str("recorded copy facts do not match the planned replacement")
            }
            Self::SourceRecheck(error) => error.fmt(formatter),
            Self::SourceDoesNotMatchAction => {
                formatter.write_str("rechecked source does not match the planned replacement")
            }
            Self::TargetInspection(error)
            | Self::TemporaryInspection(error)
            | Self::PostconditionInspection(error) => error.fmt(formatter),
            Self::Filesystem(error) => {
                write!(formatter, "copy filesystem operation failed: {error}")
            }
            Self::PreconditionNoLongerHolds { observation } => write!(
                formatter,
                "copy target precondition no longer holds: {observation:?}"
            ),
            Self::PostconditionNotMet => {
                formatter.write_str("copy replacement postcondition was not met")
            }
        }
    }
}

impl std::error::Error for ReplaceCopyExecutionError {}

/// The reason a selected `remove_copy` action could not be proven successful.
#[derive(Debug)]
pub(crate) enum RemoveCopyExecutionError {
    UnsupportedAction,
    MissingPrevious,
    TargetInspection(TargetInspectionError),
    Filesystem(io::Error),
    PreconditionNoLongerHolds { observation: CopyTargetObservation },
    PostconditionInspection(TargetInspectionError),
    PostconditionNotMet { observation: CopyTargetObservation },
}

impl fmt::Display for RemoveCopyExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAction => formatter.write_str("copy executor requires remove_copy"),
            Self::MissingPrevious => formatter.write_str("remove_copy has no previous copy fact"),
            Self::TargetInspection(error) | Self::PostconditionInspection(error) => {
                error.fmt(formatter)
            }
            Self::Filesystem(error) => {
                write!(formatter, "copy filesystem operation failed: {error}")
            }
            Self::PreconditionNoLongerHolds { observation } => write!(
                formatter,
                "copy target precondition no longer holds: {observation:?}"
            ),
            Self::PostconditionNotMet { observation } => write!(
                formatter,
                "copy removal postcondition was not met: {observation:?}"
            ),
        }
    }
}

impl std::error::Error for RemoveCopyExecutionError {}

/// The reason a selected `relocate_copy` action could not be proven successful.
#[derive(Debug)]
pub(crate) enum RelocateCopyExecutionError {
    UnsupportedAction,
    MissingFacts,
    RecordDoesNotMatchAction,
    SourceRecheck(SourceVerificationError),
    SourceDoesNotMatchAction,
    OldTargetInspection(TargetInspectionError),
    NewTargetInspection(TargetInspectionError),
    TemporaryInspection(TargetInspectionError),
    Filesystem(io::Error),
    PreconditionNoLongerHolds {
        old_observation: CopyTargetObservation,
        new_observation: CopyTargetObservation,
    },
    NewPostconditionNotMet {
        observation: CopyTargetObservation,
    },
    PostconditionNotMet {
        old_observation: CopyTargetObservation,
        temporary_observation: CopyTargetObservation,
    },
}

impl fmt::Display for RelocateCopyExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAction => formatter
                .write_str("copy executor requires matching relocate_copy action and record"),
            Self::MissingFacts => {
                formatter.write_str("relocate_copy is missing required planned or recorded facts")
            }
            Self::RecordDoesNotMatchAction => {
                formatter.write_str("recorded copy facts do not match the planned relocation")
            }
            Self::SourceRecheck(error) => error.fmt(formatter),
            Self::SourceDoesNotMatchAction => {
                formatter.write_str("rechecked source does not match the planned relocation")
            }
            Self::OldTargetInspection(error)
            | Self::NewTargetInspection(error)
            | Self::TemporaryInspection(error) => error.fmt(formatter),
            Self::Filesystem(error) => {
                write!(formatter, "copy filesystem operation failed: {error}")
            }
            Self::PreconditionNoLongerHolds {
                old_observation,
                new_observation,
            } => write!(
                formatter,
                "copy relocation precondition no longer holds: old={old_observation:?}, new={new_observation:?}"
            ),
            Self::NewPostconditionNotMet { observation } => write!(
                formatter,
                "new copy target was not published: {observation:?}"
            ),
            Self::PostconditionNotMet {
                old_observation,
                temporary_observation,
            } => write!(
                formatter,
                "copy relocation postcondition was not met: old={old_observation:?}, temporary={temporary_observation:?}"
            ),
        }
    }
}

impl std::error::Error for RelocateCopyExecutionError {}

/// The reason a managed link-to-copy handoff could not be proven successful.
#[derive(Debug)]
pub(crate) enum LinkToCopyHandoffExecutionError {
    InvalidEffectPair,
    MissingRecordedFacts,
    RecordDoesNotMatchAction,
    SourceRecheck(SourceVerificationError),
    SourceDoesNotMatchAction,
    TargetInspection(TargetInspectionError),
    TemporaryInspection(TargetInspectionError),
    Filesystem(io::Error),
    PreconditionNoLongerHolds,
    PostconditionInspection(TargetInspectionError),
    PostconditionNotMet,
}

impl fmt::Display for LinkToCopyHandoffExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEffectPair => formatter
                .write_str("handoff executor requires a link old effect and copy final effect"),
            Self::MissingRecordedFacts => {
                formatter.write_str("link-to-copy handoff has no recorded facts")
            }
            Self::RecordDoesNotMatchAction => {
                formatter.write_str("recorded handoff facts do not match the planned handoff")
            }
            Self::SourceRecheck(error) => error.fmt(formatter),
            Self::SourceDoesNotMatchAction => {
                formatter.write_str("rechecked source does not match the final copy")
            }
            Self::TargetInspection(error)
            | Self::TemporaryInspection(error)
            | Self::PostconditionInspection(error) => error.fmt(formatter),
            Self::Filesystem(error) => {
                write!(formatter, "copy filesystem operation failed: {error}")
            }
            Self::PreconditionNoLongerHolds => {
                formatter.write_str("old managed link precondition no longer holds")
            }
            Self::PostconditionNotMet => {
                formatter.write_str("link-to-copy handoff postcondition was not met")
            }
        }
    }
}

impl std::error::Error for LinkToCopyHandoffExecutionError {}

/// The reason a managed copy-to-link handoff could not be proven successful.
#[derive(Debug)]
pub(crate) enum CopyToLinkHandoffExecutionError {
    InvalidEffectPair,
    MissingRecordedFacts,
    RecordDoesNotMatchAction,
    SourceRecheck(SourceVerificationError),
    SourceDoesNotMatchAction,
    TargetInspection(TargetInspectionError),
    TemporaryInspection(TargetInspectionError),
    Filesystem(io::Error),
    PreconditionNoLongerHolds,
    PostconditionInspection(TargetInspectionError),
    PostconditionNotMet,
}

impl fmt::Display for CopyToLinkHandoffExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEffectPair => formatter
                .write_str("handoff executor requires a copy old effect and link final effect"),
            Self::MissingRecordedFacts => {
                formatter.write_str("copy-to-link handoff has no recorded facts")
            }
            Self::RecordDoesNotMatchAction => {
                formatter.write_str("recorded handoff facts do not match the planned handoff")
            }
            Self::SourceRecheck(error) => error.fmt(formatter),
            Self::SourceDoesNotMatchAction => {
                formatter.write_str("rechecked source does not match the final link")
            }
            Self::TargetInspection(error)
            | Self::TemporaryInspection(error)
            | Self::PostconditionInspection(error) => error.fmt(formatter),
            Self::Filesystem(error) => {
                write!(formatter, "copy filesystem operation failed: {error}")
            }
            Self::PreconditionNoLongerHolds => {
                formatter.write_str("old managed copy precondition no longer holds")
            }
            Self::PostconditionNotMet => {
                formatter.write_str("copy-to-link handoff postcondition was not met")
            }
        }
    }
}

impl std::error::Error for CopyToLinkHandoffExecutionError {}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::domain::desired::ResolvedResource;
    use crate::domain::file_copy::{ContentFingerprint, ResolvedFileCopy};
    use crate::domain::file_link::ResolvedFileLink;
    use crate::domain::ids::FullyQualifiedResourceId;
    use crate::domain::known::{KnownFileCopy, KnownFileLink, KnownResource};
    use crate::domain::paths::{ResolvedPath, SourceRelativePath};
    use crate::domain::plan::{PlannedEffectHandoff, TargetCondition};
    use crate::inspection::source::{resolve_store_root, verify_regular_source};
    use crate::state::operation::{
        ActionStatus, PersistedCopyActionFacts, PersistedEffectHandoffFacts,
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn fingerprint(bytes: &[u8]) -> ContentFingerprint {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        ContentFingerprint::parse(format!("sha256:{:x}", hasher.finalize())).unwrap()
    }

    #[test]
    fn create_uses_only_the_recorded_temporary_and_verifies_its_postcondition() {
        let root = std::env::temp_dir().join(format!(
            "loadout-copy-executor-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::create_dir(root.join("store")).unwrap();
        fs::write(root.join("store/config"), b"copy source bytes\n").unwrap();
        let source_root = resolve_store_root(&root.join("store")).unwrap();
        let source =
            verify_regular_source(&source_root, &SourceRelativePath::parse("config").unwrap())
                .unwrap();
        let target = ResolvedPath::new(root.join("home/.config")).unwrap();
        let temporary = ResolvedPath::new(root.join("home/.loadout-copy-a1")).unwrap();
        let desired = ResolvedFileCopy::new(
            FullyQualifiedResourceId::parse("base/config").unwrap(),
            source.path().clone(),
            target.clone(),
            fingerprint(b"copy source bytes\n"),
        )
        .unwrap();
        let action = PlannedFileCopyAction::Create {
            desired: desired.clone(),
        };
        let final_effect = KnownFileCopy::from_resolved(&desired);
        let recorded = RecordedAction::from_persisted_copy(PersistedCopyActionFacts {
            kind: ActionKind::CreateCopy,
            resource_id: desired.resource_id().clone(),
            source_path: desired.source_path().clone(),
            target_path: target.clone(),
            content_fingerprint: desired.source_content_fingerprint().clone(),
            temporary_path: temporary.clone(),
            old_effect: None,
            final_effect: KnownResource::from(final_effect),
            precondition: TargetCondition::Missing {
                target_path: target.clone(),
            },
            postcondition: TargetCondition::ExpectedCopy {
                target_path: target.clone(),
                content_fingerprint: desired.source_content_fingerprint().clone(),
            },
            status: ActionStatus::Running,
        })
        .unwrap();

        let executor = FileCopyExecutor::new(&root.join("home")).unwrap();
        executor
            .preflight(
                &PlannedResourceAction::FileCopy(action.clone()),
                Some(&source),
            )
            .unwrap();
        executor
            .execute_create(&action, &recorded, &source)
            .unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"copy source bytes\n");
        assert!(fs::symlink_metadata(&temporary).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preflight_rejects_changed_copy_source_without_creating_the_target() {
        let root = std::env::temp_dir().join(format!(
            "loadout-copy-preflight-source-change-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::create_dir(root.join("store")).unwrap();
        fs::write(root.join("store/config"), b"planned bytes\n").unwrap();
        let source_root = resolve_store_root(&root.join("store")).unwrap();
        let source =
            verify_regular_source(&source_root, &SourceRelativePath::parse("config").unwrap())
                .unwrap();
        let target = ResolvedPath::new(root.join("home/.config")).unwrap();
        let desired = ResolvedFileCopy::new(
            FullyQualifiedResourceId::parse("base/config").unwrap(),
            source.path().clone(),
            target.clone(),
            fingerprint(b"planned bytes\n"),
        )
        .unwrap();
        fs::write(root.join("store/config"), b"changed bytes\n").unwrap();

        let executor = FileCopyExecutor::new(&root.join("home")).unwrap();
        let error = executor
            .preflight(
                &PlannedResourceAction::FileCopy(PlannedFileCopyAction::Create { desired }),
                Some(&source),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            CopyPreflightError::SourceFingerprintChanged
        ));
        assert!(fs::symlink_metadata(&target).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn replace_requires_the_recorded_old_copy_and_verifies_the_new_copy() {
        let root = std::env::temp_dir().join(format!(
            "loadout-copy-executor-replace-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::create_dir(root.join("store")).unwrap();
        fs::write(root.join("store/config"), b"replacement source bytes\n").unwrap();
        let source_root = resolve_store_root(&root.join("store")).unwrap();
        let source =
            verify_regular_source(&source_root, &SourceRelativePath::parse("config").unwrap())
                .unwrap();
        let target = ResolvedPath::new(root.join("home/.config")).unwrap();
        let temporary = ResolvedPath::new(root.join("home/.loadout-copy-a1")).unwrap();
        fs::write(&target, b"old owned bytes\n").unwrap();
        let previous = KnownFileCopy::new(
            FullyQualifiedResourceId::parse("base/config").unwrap(),
            source.path().clone(),
            target.clone(),
            fingerprint(b"old owned bytes\n"),
        )
        .unwrap();
        let desired = ResolvedFileCopy::new(
            previous.resource_id().clone(),
            source.path().clone(),
            target.clone(),
            fingerprint(b"replacement source bytes\n"),
        )
        .unwrap();
        let action = PlannedFileCopyAction::Replace {
            desired: desired.clone(),
            previous: previous.clone(),
        };
        let recorded = RecordedAction::from_persisted_copy(PersistedCopyActionFacts {
            kind: ActionKind::ReplaceCopy,
            resource_id: desired.resource_id().clone(),
            source_path: desired.source_path().clone(),
            target_path: target.clone(),
            content_fingerprint: desired.source_content_fingerprint().clone(),
            temporary_path: temporary.clone(),
            old_effect: Some(KnownResource::from(previous)),
            final_effect: KnownResource::from(KnownFileCopy::from_resolved(&desired)),
            precondition: TargetCondition::ExpectedCopy {
                target_path: target.clone(),
                content_fingerprint: fingerprint(b"old owned bytes\n"),
            },
            postcondition: TargetCondition::ExpectedCopy {
                target_path: target.clone(),
                content_fingerprint: desired.source_content_fingerprint().clone(),
            },
            status: ActionStatus::Running,
        })
        .unwrap();

        FileCopyExecutor::new(&root.join("home"))
            .unwrap()
            .execute_replace(&action, &recorded, &source)
            .unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"replacement source bytes\n");
        assert!(fs::symlink_metadata(&temporary).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remove_requires_the_exact_owned_fingerprint() {
        let root = std::env::temp_dir().join(format!(
            "loadout-copy-executor-remove-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        let target = ResolvedPath::new(root.join("home/.config")).unwrap();
        fs::write(&target, b"owned copy bytes\n").unwrap();
        let previous = KnownFileCopy::new(
            FullyQualifiedResourceId::parse("base/config").unwrap(),
            ResolvedPath::new(root.join("store/config")).unwrap(),
            target.clone(),
            fingerprint(b"owned copy bytes\n"),
        )
        .unwrap();
        let action = PlannedFileCopyAction::Remove {
            previous: previous.clone(),
        };

        FileCopyExecutor::new(&root.join("home"))
            .unwrap()
            .execute_remove(&action)
            .unwrap();

        assert!(fs::symlink_metadata(&target).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn relocate_publishes_the_new_copy_before_removing_the_old_copy() {
        let root = std::env::temp_dir().join(format!(
            "loadout-copy-executor-relocate-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::create_dir(root.join("store")).unwrap();
        fs::create_dir_all(root.join("home/config")).unwrap();
        fs::write(root.join("store/config"), b"relocated source bytes\n").unwrap();
        let source_root = resolve_store_root(&root.join("store")).unwrap();
        let source =
            verify_regular_source(&source_root, &SourceRelativePath::parse("config").unwrap())
                .unwrap();
        let old_target = ResolvedPath::new(root.join("home/.config")).unwrap();
        let new_target = ResolvedPath::new(root.join("home/config/config")).unwrap();
        let temporary = ResolvedPath::new(root.join("home/config/.loadout-copy-a1")).unwrap();
        fs::write(&old_target, b"old owned bytes\n").unwrap();
        let previous = KnownFileCopy::new(
            FullyQualifiedResourceId::parse("base/config").unwrap(),
            source.path().clone(),
            old_target.clone(),
            fingerprint(b"old owned bytes\n"),
        )
        .unwrap();
        let desired = ResolvedFileCopy::new(
            previous.resource_id().clone(),
            source.path().clone(),
            new_target.clone(),
            fingerprint(b"relocated source bytes\n"),
        )
        .unwrap();
        let action = PlannedFileCopyAction::Relocate {
            desired: desired.clone(),
            previous: previous.clone(),
        };
        let recorded = RecordedAction::from_persisted_copy(PersistedCopyActionFacts {
            kind: ActionKind::RelocateCopy,
            resource_id: desired.resource_id().clone(),
            source_path: desired.source_path().clone(),
            target_path: new_target.clone(),
            content_fingerprint: desired.source_content_fingerprint().clone(),
            temporary_path: temporary.clone(),
            old_effect: Some(KnownResource::from(previous.clone())),
            final_effect: KnownResource::from(KnownFileCopy::from_resolved(&desired)),
            precondition: TargetCondition::ExpectedCopy {
                target_path: old_target.clone(),
                content_fingerprint: previous.content_fingerprint().clone(),
            },
            postcondition: TargetCondition::ExpectedCopy {
                target_path: new_target.clone(),
                content_fingerprint: desired.source_content_fingerprint().clone(),
            },
            status: ActionStatus::Running,
        })
        .unwrap();

        FileCopyExecutor::new(&root.join("home"))
            .unwrap()
            .execute_relocate(&action, &recorded, &source)
            .unwrap();

        assert!(fs::symlink_metadata(&old_target).is_err());
        assert_eq!(fs::read(&new_target).unwrap(), b"relocated source bytes\n");
        assert!(fs::symlink_metadata(&temporary).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn link_to_copy_handoff_replaces_only_the_expected_managed_link() {
        let root = std::env::temp_dir().join(format!(
            "loadout-copy-executor-handoff-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::create_dir(root.join("store")).unwrap();
        fs::write(root.join("store/old"), b"old link source\n").unwrap();
        fs::write(root.join("store/config"), b"final copy bytes\n").unwrap();
        let source_root = resolve_store_root(&root.join("store")).unwrap();
        let source =
            verify_regular_source(&source_root, &SourceRelativePath::parse("config").unwrap())
                .unwrap();
        let target = ResolvedPath::new(root.join("home/.config")).unwrap();
        let temporary = ResolvedPath::new(root.join("home/.loadout-effect-a1")).unwrap();
        let old_link = ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/config").unwrap(),
            ResolvedPath::new(root.join("store/old")).unwrap(),
            target.clone(),
        )
        .unwrap();
        symlink(old_link.link_target().as_path(), &target).unwrap();
        let final_copy = ResolvedFileCopy::new(
            old_link.resource_id().clone(),
            source.path().clone(),
            target.clone(),
            fingerprint(b"final copy bytes\n"),
        )
        .unwrap();
        let action = PlannedEffectHandoff::new(
            KnownResource::from(KnownFileLink::from_resolved(&old_link)),
            ResolvedResource::from(final_copy.clone()),
        )
        .unwrap();
        let recorded = RecordedAction::from_persisted_replace_effect(PersistedEffectHandoffFacts {
            resource_id: old_link.resource_id().clone(),
            old_effect: KnownResource::from(KnownFileLink::from_resolved(&old_link)),
            final_effect: KnownResource::from(KnownFileCopy::from_resolved(&final_copy)),
            temporary_path: temporary.clone(),
            precondition: TargetCondition::ExpectedLink {
                target_path: target.clone(),
                link_target: old_link.link_target().clone(),
            },
            postcondition: TargetCondition::ExpectedCopy {
                target_path: target.clone(),
                content_fingerprint: final_copy.source_content_fingerprint().clone(),
            },
            status: ActionStatus::Running,
        })
        .unwrap();

        FileCopyExecutor::new(&root.join("home"))
            .unwrap()
            .execute_link_to_copy_handoff(&action, &recorded, &source)
            .unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"final copy bytes\n");
        assert!(
            !fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(fs::symlink_metadata(&temporary).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn copy_to_link_handoff_replaces_only_the_expected_managed_copy() {
        let root = std::env::temp_dir().join(format!(
            "loadout-copy-executor-handoff-reverse-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::create_dir(root.join("store")).unwrap();
        fs::write(root.join("store/config"), b"final link source\n").unwrap();
        let source_root = resolve_store_root(&root.join("store")).unwrap();
        let source =
            verify_regular_source(&source_root, &SourceRelativePath::parse("config").unwrap())
                .unwrap();
        let target = ResolvedPath::new(root.join("home/.config")).unwrap();
        let temporary = ResolvedPath::new(root.join("home/.loadout-effect-a1")).unwrap();
        fs::write(&target, b"old owned copy\n").unwrap();
        let old_copy = KnownFileCopy::new(
            FullyQualifiedResourceId::parse("base/config").unwrap(),
            source.path().clone(),
            target.clone(),
            fingerprint(b"old owned copy\n"),
        )
        .unwrap();
        let final_link = ResolvedFileLink::new(
            old_copy.resource_id().clone(),
            source.path().clone(),
            target.clone(),
        )
        .unwrap();
        let action = PlannedEffectHandoff::new(
            KnownResource::from(old_copy.clone()),
            ResolvedResource::from(final_link.clone()),
        )
        .unwrap();
        let recorded = RecordedAction::from_persisted_replace_effect(PersistedEffectHandoffFacts {
            resource_id: old_copy.resource_id().clone(),
            old_effect: KnownResource::from(old_copy),
            final_effect: KnownResource::from(KnownFileLink::from_resolved(&final_link)),
            temporary_path: temporary.clone(),
            precondition: TargetCondition::ExpectedCopy {
                target_path: target.clone(),
                content_fingerprint: fingerprint(b"old owned copy\n"),
            },
            postcondition: TargetCondition::ExpectedLink {
                target_path: target.clone(),
                link_target: final_link.link_target().clone(),
            },
            status: ActionStatus::Running,
        })
        .unwrap();

        FileCopyExecutor::new(&root.join("home"))
            .unwrap()
            .execute_copy_to_link_handoff(&action, &recorded, &source)
            .unwrap();

        assert_eq!(fs::read_link(&target).unwrap(), source.path().as_ref());
        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(fs::symlink_metadata(&temporary).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
