//! Strict state-file persistence, exclusive locking, and operation transitions.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::hashes::{CanonicalHashError, DesiredHash};
use crate::domain::known::{KnownFileLink, KnownState, KnownStateError};
use crate::domain::paths::ResolvedPath;
use crate::domain::plan::{ActionKind, PlannedAction};
pub(crate) use crate::state::codec::StateDecodeError;
use crate::state::codec::StateDocument;
use crate::state::operation::{
    ActionId, ActionStatus, OperationId, OperationRecord, OperationRecordError,
    RecordedKnownStateUpdate,
};

const STATE_FILE_NAME: &str = "state.json";
const LOCK_FILE_NAME: &str = "state.lock";
const MAX_TEMPORARY_NAME_ATTEMPTS: u64 = 128;

static NEXT_OPERATION_NONCE: AtomicU64 = AtomicU64::new(0);
static NEXT_TEMPORARY_NONCE: AtomicU64 = AtomicU64::new(0);
static NEXT_REPLACEMENT_NONCE: AtomicU64 = AtomicU64::new(0);

/// A complete in-memory state snapshot validated against the v0.2.0 state schema.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PersistedState {
    known: KnownState,
    active_operation: Option<OperationRecord>,
}

impl PersistedState {
    /// Accepts decoded facts only after checking operation/Known consistency.
    pub(super) fn from_parts(
        known: KnownState,
        active_operation: Option<OperationRecord>,
    ) -> Result<Self, StateDecodeError> {
        if let Some(operation) = &active_operation {
            for (_, action) in operation.actions() {
                if let Some(facts) = action.replacement_facts() {
                    if known
                        .resources()
                        .any(|resource| resource.target_path() == facts.temporary_path())
                    {
                        return Err(StateDecodeError::InvalidOperation(
                            OperationRecordError::DuplicateTargetPath {
                                target_path: facts.temporary_path().clone(),
                            },
                        ));
                    }
                }
            }
            validate_succeeded_actions(&known, operation)?;
            validate_unfinished_stale_action_known_state(&known, operation)?;
        }
        Ok(Self {
            known,
            active_operation,
        })
    }

    fn empty() -> Self {
        Self::default()
    }

    /// Verified historical facts supplied to the planner.
    pub(crate) fn known(&self) -> &KnownState {
        &self.known
    }

    /// A started operation whose result may need recovery, if any.
    pub(crate) fn active_operation(&self) -> Option<&OperationRecord> {
        self.active_operation.as_ref()
    }
}

/// Access point for one platform state directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StateRepository {
    state_directory: ResolvedPath,
    #[cfg(test)]
    fail_state_write_preflight: bool,
}

impl StateRepository {
    /// Binds a repository to the resolved platform state directory without I/O.
    pub(crate) fn new(state_directory: ResolvedPath) -> Self {
        Self {
            state_directory,
            #[cfg(test)]
            fail_state_write_preflight: false,
        }
    }

    /// Loads and validates state without creating a directory, lock, or state file.
    pub(crate) fn load(&self) -> Result<PersistedState, StateRepositoryError> {
        read_state_file(&self.state_file_path())
    }

    /// Creates the state directory and holds the exclusive operating-system lock.
    pub(crate) fn acquire_exclusive(&self) -> Result<LockedStateRepository, StateRepositoryError> {
        #[cfg(test)]
        crate::test_support::assert_mutation_allowed();
        fs::create_dir_all(self.state_directory.as_ref()).map_err(|source| {
            StateRepositoryError::StateDirectoryIo {
                path: self.state_directory.as_ref().to_path_buf(),
                source,
            }
        })?;
        let directory_metadata = fs::metadata(self.state_directory.as_ref()).map_err(|source| {
            StateRepositoryError::StateDirectoryIo {
                path: self.state_directory.as_ref().to_path_buf(),
                source,
            }
        })?;
        if !directory_metadata.is_dir() {
            return Err(StateRepositoryError::StateDirectoryNotDirectory {
                path: self.state_directory.as_ref().to_path_buf(),
            });
        }

        let lock_path = self.lock_file_path();
        let lock = ExclusiveStateLock::acquire(&lock_path)?;
        let state = self.load()?;
        Ok(LockedStateRepository {
            repository: self.clone(),
            state,
            _lock: lock,
            last_closed_operation: None,
            #[cfg(test)]
            next_commit_fault: None,
            #[cfg(test)]
            commits_before_fault: 0,
            #[cfg(test)]
            fail_state_write_preflight: self.fail_state_write_preflight,
        })
    }

    fn state_file_path(&self) -> PathBuf {
        self.state_directory.as_ref().join(STATE_FILE_NAME)
    }

    fn lock_file_path(&self) -> PathBuf {
        self.state_directory.as_ref().join(LOCK_FILE_NAME)
    }

    #[cfg(test)]
    pub(crate) fn fail_next_state_write_preflight(&mut self) {
        self.fail_state_write_preflight = true;
    }
}

/// Operation facts captured from a locked session, without another state-file read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OperationOutcome {
    Absent,
    Retained(OperationRecord),
    Closed(OperationRecord),
}

/// Whether a failed commit replaced the authoritative state file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommitFailureEffect {
    PreviousStateRetained,
    ReplacedDurabilityUnconfirmed,
}

/// A repository session that owns the exclusive state lock and its validated state.
pub(crate) struct LockedStateRepository {
    repository: StateRepository,
    state: PersistedState,
    _lock: ExclusiveStateLock,
    last_closed_operation: Option<OperationRecord>,
    #[cfg(test)]
    next_commit_fault: Option<CommitStage>,
    #[cfg(test)]
    commits_before_fault: usize,
    #[cfg(test)]
    fail_state_write_preflight: bool,
}

impl LockedStateRepository {
    /// Returns the latest complete state committed through this lock session.
    pub(crate) fn state(&self) -> &PersistedState {
        &self.state
    }

    pub(crate) fn operation_outcome(&self) -> OperationOutcome {
        match (&self.state.active_operation, &self.last_closed_operation) {
            (Some(operation), _) => OperationOutcome::Retained(operation.clone()),
            (None, Some(operation)) => OperationOutcome::Closed(operation.clone()),
            (None, None) => OperationOutcome::Absent,
        }
    }

    /// Proves that the locked repository can use its durable-state channel without creating an operation record or changing `state.json`.
    ///
    /// A read-only state directory or state file is rejected before confirmation. These are snapshots: ACL, permission, or storage changes can still occur later and are handled by the operation-record protocol.
    pub(crate) fn preflight_writable(&mut self) -> Result<(), StateRepositoryError> {
        #[cfg(test)]
        crate::test_support::assert_mutation_allowed();
        #[cfg(test)]
        if std::mem::take(&mut self.fail_state_write_preflight) {
            return Err(StateRepositoryError::StateWritePreflight {
                state_directory: self.repository.state_directory.as_ref().to_path_buf(),
                source: io::Error::other("injected state write preflight failure"),
            });
        }

        self._lock
            .file
            .sync_all()
            .map_err(|source| StateRepositoryError::StateWritePreflight {
                state_directory: self.repository.state_directory.as_ref().to_path_buf(),
                source,
            })?;

        let directory_metadata =
            fs::metadata(self.repository.state_directory.as_ref()).map_err(|source| {
                StateRepositoryError::StateWritePreflight {
                    state_directory: self.repository.state_directory.as_ref().to_path_buf(),
                    source,
                }
            })?;
        if directory_metadata.permissions().readonly() {
            return Err(StateRepositoryError::StateWritePreflight {
                state_directory: self.repository.state_directory.as_ref().to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the state directory is read-only",
                ),
            });
        }

        match OpenOptions::new()
            .write(true)
            .open(self.repository.state_file_path())
        {
            Ok(file) => {
                file.sync_all()
                    .map_err(|source| StateRepositoryError::StateWritePreflight {
                        state_directory: self.repository.state_directory.as_ref().to_path_buf(),
                        source,
                    })
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StateRepositoryError::StateWritePreflight {
                state_directory: self.repository.state_directory.as_ref().to_path_buf(),
                source,
            }),
        }
    }

    /// Persists a fresh pending single-action operation before its executor work begins.
    pub(crate) fn begin_operation(
        &mut self,
        desired_hash: DesiredHash,
        action: &PlannedAction,
    ) -> Result<ActionId, StateRepositoryError> {
        let ids = self.begin_actions(desired_hash, std::slice::from_ref(action))?;
        Ok(ids[0].clone())
    }

    /// Records the complete execution sequence in one atomic commit. Returned IDs correspond to the supplied sequence; persisted object order is irrelevant.
    pub(crate) fn begin_actions(
        &mut self,
        desired_hash: DesiredHash,
        actions: &[PlannedAction],
    ) -> Result<Vec<ActionId>, StateRepositoryError> {
        if self.state.active_operation.is_some() {
            return Err(StateRepositoryError::ActiveOperationPresent);
        }
        let mut reserved = self
            .state
            .known()
            .resources()
            .map(|resource| resource.target_path().clone())
            .collect::<BTreeSet<_>>();
        for action in actions {
            for condition in action
                .preconditions()
                .into_iter()
                .chain(action.postconditions())
            {
                reserved.insert(condition.target_path().clone());
            }
        }
        let mut recorded = Vec::new();
        let mut ids = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            let id = ActionId::parse(format!("a{}", index + 1))
                .map_err(StateRepositoryError::Operation)?;
            let facts = match action.kind() {
                ActionKind::ReplaceLink => crate::state::operation::RecordedAction::replace_link(
                    action,
                    self.allocate_replacement_temporary_path(action, &reserved)?,
                ),
                ActionKind::ReplaceOwnership => {
                    let temporary = if action.preconditions() == action.postconditions() {
                        None
                    } else {
                        Some(self.allocate_replacement_temporary_path(action, &reserved)?)
                    };
                    crate::state::operation::RecordedAction::replace_ownership(action, temporary)
                }
                ActionKind::RelocateLink => {
                    crate::state::operation::RecordedAction::relocate_link(action)
                }
                _ => crate::state::operation::RecordedAction::from_action(action),
            }
            .map_err(StateRepositoryError::Operation)?;
            if let Some(facts) = facts.replacement_facts() {
                reserved.insert(facts.temporary_path().clone());
            }
            ids.push(id.clone());
            recorded.push((id, facts));
        }
        let operation = OperationRecord::from_actions(new_operation_id(), desired_hash, recorded)
            .map_err(StateRepositoryError::Operation)?;
        let mut candidate = self.state.clone();
        candidate.active_operation = Some(operation);
        self.commit_candidate(candidate)?;
        Ok(ids)
    }

    fn allocate_replacement_temporary_path(
        &self,
        action: &PlannedAction,
        reserved: &BTreeSet<ResolvedPath>,
    ) -> Result<ResolvedPath, StateRepositoryError> {
        let target = action.preconditions().into_iter().next().ok_or_else(|| {
            StateRepositoryError::Operation(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            })
        })?;
        let parent = target.target_path().as_ref().parent().ok_or_else(|| {
            StateRepositoryError::Operation(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            })
        })?;
        for _ in 0..MAX_TEMPORARY_NAME_ATTEMPTS {
            let nonce = NEXT_REPLACEMENT_NONCE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".loadout-replace-{}-{nonce}", std::process::id()));
            if reserved.iter().any(|reserved| reserved.as_ref() == path) {
                continue;
            }
            match fs::symlink_metadata(&path) {
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    return ResolvedPath::new(path).map_err(|_| {
                        StateRepositoryError::Operation(
                            OperationRecordError::InvalidActionConditions {
                                kind: action.kind(),
                            },
                        )
                    });
                }
                Ok(_) => continue,
                Err(source) => {
                    return Err(StateRepositoryError::StateDirectoryIo {
                        path: path.clone(),
                        source,
                    });
                }
            }
        }
        Err(StateRepositoryError::Operation(
            OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            },
        ))
    }

    /// Compatibility entry point for Slice 4's create-only coordinator.
    pub(crate) fn begin_create_operation(
        &mut self,
        desired_hash: DesiredHash,
        action: &PlannedAction,
    ) -> Result<ActionId, StateRepositoryError> {
        self.begin_operation(desired_hash, action)
    }

    /// Atomically records `running` before the executor attempts a mutation.
    pub(crate) fn mark_running(
        &mut self,
        action_id: &ActionId,
    ) -> Result<(), StateRepositoryError> {
        let mut candidate = self.state.clone();
        let operation = candidate
            .active_operation
            .as_mut()
            .ok_or(StateRepositoryError::NoActiveOperation)?;
        operation
            .mark_running(action_id)
            .map_err(StateRepositoryError::Operation)?;
        self.commit_candidate(candidate)
    }

    /// Atomically records a conclusive failed or uncertain result without changing Known state.
    pub(crate) fn mark_without_known(
        &mut self,
        action_id: &ActionId,
        status: ActionStatus,
    ) -> Result<(), StateRepositoryError> {
        let mut candidate = self.state.clone();
        let operation = candidate
            .active_operation
            .as_mut()
            .ok_or(StateRepositoryError::NoActiveOperation)?;
        operation
            .mark_without_known(action_id, status)
            .map_err(StateRepositoryError::Operation)?;
        self.commit_candidate(candidate)
    }

    /// Atomically commits a verified action's exact Known-state update and `succeeded` status.
    pub(crate) fn commit_succeeded(
        &mut self,
        action_id: &ActionId,
    ) -> Result<(), StateRepositoryError> {
        let mut candidate = self.state.clone();
        let update = candidate
            .active_operation
            .as_mut()
            .ok_or(StateRepositoryError::NoActiveOperation)?
            .mark_succeeded(action_id)
            .map_err(StateRepositoryError::Operation)?;
        candidate.known = match update {
            RecordedKnownStateUpdate::Upsert(known) => candidate
                .known
                .with_upserted(known)
                .map_err(StateRepositoryError::KnownState)?,
            RecordedKnownStateUpdate::RemoveExpected(known) => candidate
                .known
                .with_removed(&known)
                .map_err(StateRepositoryError::KnownState)?,
            RecordedKnownStateUpdate::RemoveMissing { resource_id } => candidate
                .known
                .with_missing_resource_removed(&resource_id)
                .map_err(StateRepositoryError::KnownState)?,
            RecordedKnownStateUpdate::ReplaceIdentity {
                old_resource,
                new_resource,
            } => candidate
                .known
                .with_replaced_identity(&old_resource, new_resource)
                .map_err(StateRepositoryError::KnownState)?,
        };
        self.commit_candidate(candidate)
    }

    /// Compatibility entry point for Slice 4's create-only coordinator.
    pub(crate) fn commit_create_succeeded(
        &mut self,
        action_id: &ActionId,
    ) -> Result<(), StateRepositoryError> {
        self.commit_succeeded(action_id)
    }

    /// Removes a completed operation only when no action remains pending, running, or uncertain.
    pub(crate) fn close_finished_operation(&mut self) -> Result<(), StateRepositoryError> {
        let mut candidate = self.state.clone();
        let operation = candidate
            .active_operation
            .as_ref()
            .ok_or(StateRepositoryError::NoActiveOperation)?;
        if !operation.can_close() {
            return Err(StateRepositoryError::OperationNotCloseable);
        }
        let closed = operation.clone();
        candidate.active_operation = None;
        let result = self.commit_candidate(candidate);
        // The state may have been replaced even when its directory flush failed.
        if self.state.active_operation.is_none() {
            self.last_closed_operation = Some(closed);
        }
        result
    }

    fn commit_candidate(&mut self, candidate: PersistedState) -> Result<(), StateRepositoryError> {
        match self.write_state_atomically(&candidate) {
            Ok(()) => {
                self.state = candidate;
                Ok(())
            }
            Err(error) if error.replacement_completed() => {
                // A directory flush can report an error after the atomic replacement.
                // Keep memory aligned with the complete state file while surfacing that its final durability could not be proven.
                self.state = candidate;
                Err(StateRepositoryError::Commit(error))
            }
            Err(error) => Err(StateRepositoryError::Commit(error)),
        }
    }

    fn write_state_atomically(&mut self, state: &PersistedState) -> Result<(), CommitError> {
        #[cfg(test)]
        crate::test_support::assert_mutation_allowed();
        let document = StateDocument::from_state(state)?;
        let encoded = serde_json::to_vec(&document).map_err(CommitError::Serialize)?;
        let temporary_path = self.unique_temporary_path()?;
        let result = (|| {
            #[cfg(all(test, unix))]
            crate::test_support::execution_boundary(
                crate::test_support::ExecutionBoundary::BeforeCommit,
            )
            .map_err(|source| CommitError::TemporaryCreate {
                path: temporary_path.clone(),
                source,
            })?;
            self.fail_at(CommitStage::CreateTemporary)?;
            let mut temporary = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
                .map_err(|source| CommitError::TemporaryCreate {
                    path: temporary_path.clone(),
                    source,
                })?;

            self.fail_at(CommitStage::WriteTemporary)?;
            temporary
                .write_all(&encoded)
                .map_err(|source| CommitError::TemporaryWrite {
                    path: temporary_path.clone(),
                    source,
                })?;

            self.fail_at(CommitStage::FlushTemporary)?;
            temporary
                .sync_all()
                .map_err(|source| CommitError::TemporaryFlush {
                    path: temporary_path.clone(),
                    source,
                })?;
            drop(temporary);

            self.fail_at(CommitStage::ReopenAndValidate)?;
            read_state_file(&temporary_path).map_err(|source| {
                CommitError::TemporaryValidation {
                    path: temporary_path.clone(),
                    source: Box::new(source),
                }
            })?;

            self.fail_at(CommitStage::ReplaceState)?;
            fs::rename(&temporary_path, self.repository.state_file_path()).map_err(|source| {
                CommitError::StateReplacement {
                    temporary_path: temporary_path.clone(),
                    state_path: self.repository.state_file_path(),
                    source,
                }
            })?;
            self.fail_at(CommitStage::FlushDirectory)?;
            flush_state_directory(self.repository.state_directory.as_ref()).map_err(|source| {
                CommitError::DirectoryFlushAfterReplacement {
                    state_directory: self.repository.state_directory.as_ref().to_path_buf(),
                    source,
                }
            })?;
            Ok(())
        })();

        // Do not remove a failed temporary path.  `create_new` establishes ownership only at creation time; an outside actor could replace the path before cleanup.  Leaving an unreferenced state-directory entry is safer than deleting an entry whose ownership can no longer be proved.  Future commits use a fresh unique temporary name.
        result
    }

    fn unique_temporary_path(&self) -> Result<PathBuf, CommitError> {
        let pid = std::process::id();
        for _ in 0..MAX_TEMPORARY_NAME_ATTEMPTS {
            let nonce = NEXT_TEMPORARY_NONCE.fetch_add(1, Ordering::Relaxed);
            let path = self
                .repository
                .state_directory
                .as_ref()
                .join(format!(".loadout-state-{pid}-{nonce}.tmp"));
            if !path.exists() {
                return Ok(path);
            }
        }
        Err(CommitError::TemporaryNameExhausted {
            state_directory: self.repository.state_directory.as_ref().to_path_buf(),
        })
    }

    #[cfg(test)]
    fn fail_at(&mut self, stage: CommitStage) -> Result<(), CommitError> {
        if self.commits_before_fault != 0 {
            if stage == CommitStage::FlushDirectory {
                self.commits_before_fault -= 1;
            }
            return Ok(());
        }
        if self.next_commit_fault == Some(stage) {
            self.next_commit_fault = None;
            return Err(CommitError::Injected { stage });
        }
        Ok(())
    }

    #[cfg(not(test))]
    fn fail_at(&mut self, _: CommitStage) -> Result<(), CommitError> {
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_commit_at(&mut self, stage: CommitStage) {
        self.next_commit_fault = Some(stage);
        self.commits_before_fault = 0;
    }

    #[cfg(test)]
    pub(crate) fn fail_commit_after(&mut self, successful_commits: usize, stage: CommitStage) {
        self.fail_next_commit_at(stage);
        self.commits_before_fault = successful_commits;
    }
}

fn read_state_file(path: &Path) -> Result<PersistedState, StateRepositoryError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(PersistedState::empty());
        }
        Err(source) => {
            return Err(StateRepositoryError::StateRead {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let document = serde_json::from_slice::<StateDocument>(&bytes).map_err(|source| {
        StateRepositoryError::InvalidStateJson {
            path: path.to_path_buf(),
            source,
        }
    })?;
    document
        .into_state()
        .map_err(|source| StateRepositoryError::InvalidState {
            path: path.to_path_buf(),
            source,
        })
}

fn flush_state_directory(state_directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(state_directory)?.sync_all()
    }
    #[cfg(windows)]
    {
        let _ = state_directory;
        Ok(())
    }
}

fn new_operation_id() -> OperationId {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = NEXT_OPERATION_NONCE.fetch_add(1, Ordering::Relaxed);
    OperationId::parse(format!("op-{}-{timestamp}-{nonce}", std::process::id()))
        .expect("generated operation IDs are always non-empty")
}

fn validate_succeeded_actions(
    known: &KnownState,
    operation: &OperationRecord,
) -> Result<(), StateDecodeError> {
    for (action_id, action) in operation.actions() {
        if action.status() != ActionStatus::Succeeded {
            continue;
        }
        let update = action
            .known_state_update_after_success()
            .map_err(StateDecodeError::InvalidOperation)?;
        let matches = match update {
            RecordedKnownStateUpdate::Upsert(expected) => {
                known.get(expected.resource_id()) == Some(&expected)
            }
            RecordedKnownStateUpdate::RemoveExpected(expected) => {
                known.get(expected.resource_id()).is_none()
            }
            RecordedKnownStateUpdate::RemoveMissing { resource_id } => {
                known.get(&resource_id).is_none()
            }
            RecordedKnownStateUpdate::ReplaceIdentity {
                old_resource,
                new_resource,
            } => {
                known.get(old_resource.resource_id()).is_none()
                    && known.get(new_resource.resource_id()) == Some(&new_resource)
            }
        };
        if !matches {
            return Err(StateDecodeError::SucceededActionKnownMismatch {
                action_id: action_id.as_str().to_owned(),
                resource_id: action.resource_id().clone(),
            });
        }
    }
    Ok(())
}

/// A pending, running, failed, skipped, or uncertain stale action must retain the exact Known fact it was authorized to remove. Otherwise a corrupted record could be completed against a different historical resource.
fn validate_unfinished_stale_action_known_state(
    known: &KnownState,
    operation: &OperationRecord,
) -> Result<(), StateDecodeError> {
    for (action_id, action) in operation.actions() {
        if action.status() == ActionStatus::Succeeded {
            continue;
        }

        let matches = match action.kind() {
            ActionKind::RemoveLink => match action
                .known_state_update_after_success()
                .map_err(StateDecodeError::InvalidOperation)?
            {
                RecordedKnownStateUpdate::RemoveExpected(expected) => {
                    known.get(expected.resource_id()) == Some(&expected)
                }
                _ => unreachable!("a validated remove action has an exact removal update"),
            },
            ActionKind::ForgetMissing => known
                .get(action.resource_id())
                .is_some_and(|resource| resource.target_path() == action.target_path()),
            ActionKind::ReplaceLink => match action.replacement_facts() {
                Some(facts) => KnownFileLink::new(
                    action.resource_id().clone(),
                    facts.old_link_target().as_path().clone(),
                    facts.target_path().clone(),
                    facts.old_link_target().clone(),
                )
                .is_ok_and(|old_resource| {
                    known.get(old_resource.resource_id()) == Some(&old_resource)
                }),
                None => false,
            },
            ActionKind::ReplaceOwnership => match action
                .known_state_update_after_success()
                .map_err(StateDecodeError::InvalidOperation)?
            {
                RecordedKnownStateUpdate::ReplaceIdentity {
                    old_resource,
                    new_resource,
                } => {
                    known.get(old_resource.resource_id()) == Some(&old_resource)
                        && known.get(new_resource.resource_id()).is_none()
                }
                _ => {
                    unreachable!("a validated ownership handoff has an identity replacement update")
                }
            },
            ActionKind::RelocateLink => match action.relocation_facts() {
                Some(facts) => KnownFileLink::new(
                    action.resource_id().clone(),
                    facts.old_link_target().as_path().clone(),
                    facts.old_target_path().clone(),
                    facts.old_link_target().clone(),
                )
                .is_ok_and(|old_resource| {
                    known.get(old_resource.resource_id()) == Some(&old_resource)
                }),
                None => false,
            },
            ActionKind::CreateLink => true,
            _ => true,
        };
        if !matches {
            return Err(StateDecodeError::ActiveActionKnownMismatch {
                action_id: action_id.as_str().to_owned(),
                resource_id: action.resource_id().clone(),
            });
        }
    }
    Ok(())
}

/// The reason state persistence or progress advancement could not proceed safely.
#[derive(Debug)]
pub(crate) enum StateRepositoryError {
    StateDirectoryIo {
        path: PathBuf,
        source: io::Error,
    },
    StateDirectoryNotDirectory {
        path: PathBuf,
    },
    StateWritePreflight {
        state_directory: PathBuf,
        source: io::Error,
    },
    LockIo {
        path: PathBuf,
        source: io::Error,
    },
    LockContended {
        path: PathBuf,
    },
    StateRead {
        path: PathBuf,
        source: io::Error,
    },
    InvalidStateJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    InvalidState {
        path: PathBuf,
        source: StateDecodeError,
    },
    ActiveOperationPresent,
    NoActiveOperation,
    OperationNotCloseable,
    Operation(OperationRecordError),
    KnownState(KnownStateError),
    Commit(CommitError),
}

impl fmt::Display for StateRepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateDirectoryIo { path, source } => {
                write!(
                    formatter,
                    "cannot access state directory {}: {source}",
                    path.display()
                )
            }
            Self::StateDirectoryNotDirectory { path } => {
                write!(
                    formatter,
                    "state directory is not a directory: {}",
                    path.display()
                )
            }
            Self::StateWritePreflight {
                state_directory,
                source,
            } => write!(
                formatter,
                "state repository is not writable in {}: {source}",
                state_directory.display()
            ),
            Self::LockIo { path, source } => {
                write!(
                    formatter,
                    "cannot acquire state lock {}: {source}",
                    path.display()
                )
            }
            Self::LockContended { path } => {
                write!(formatter, "state lock is already held: {}", path.display())
            }
            Self::StateRead { path, source } => {
                write!(
                    formatter,
                    "cannot read state file {}: {source}",
                    path.display()
                )
            }
            Self::InvalidStateJson { path, source } => {
                write!(
                    formatter,
                    "invalid state JSON in {}: {source}",
                    path.display()
                )
            }
            Self::InvalidState { path, source } => {
                write!(formatter, "invalid state in {}: {source}", path.display())
            }
            Self::ActiveOperationPresent => {
                formatter.write_str("an active operation already exists")
            }
            Self::NoActiveOperation => formatter.write_str("there is no active operation"),
            Self::OperationNotCloseable => formatter.write_str(
                "an operation with pending, running, or uncertain actions cannot be closed",
            ),
            Self::Operation(error) => error.fmt(formatter),
            Self::KnownState(error) => error.fmt(formatter),
            Self::Commit(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for StateRepositoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StateDirectoryIo { source, .. }
            | Self::StateWritePreflight { source, .. }
            | Self::LockIo { source, .. }
            | Self::StateRead { source, .. } => Some(source),
            Self::InvalidStateJson { source, .. } => Some(source),
            Self::InvalidState { source, .. } => Some(source),
            Self::Operation(error) => Some(error),
            Self::KnownState(error) => Some(error),
            Self::Commit(error) => Some(error),
            Self::StateDirectoryNotDirectory { .. }
            | Self::LockContended { .. }
            | Self::ActiveOperationPresent
            | Self::NoActiveOperation
            | Self::OperationNotCloseable => None,
        }
    }
}

/// A commit stage used by state durability tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommitStage {
    CreateTemporary,
    WriteTemporary,
    FlushTemporary,
    ReopenAndValidate,
    ReplaceState,
    FlushDirectory,
}

/// The result of failing to atomically persist a complete state document.
#[derive(Debug)]
pub(crate) enum CommitError {
    Serialize(serde_json::Error),
    Hash(CanonicalHashError),
    NonUnicodePath {
        path: PathBuf,
    },
    TemporaryNameExhausted {
        state_directory: PathBuf,
    },
    TemporaryCreate {
        path: PathBuf,
        source: io::Error,
    },
    TemporaryWrite {
        path: PathBuf,
        source: io::Error,
    },
    TemporaryFlush {
        path: PathBuf,
        source: io::Error,
    },
    TemporaryValidation {
        path: PathBuf,
        source: Box<StateRepositoryError>,
    },
    StateReplacement {
        temporary_path: PathBuf,
        state_path: PathBuf,
        source: io::Error,
    },
    DirectoryFlushAfterReplacement {
        state_directory: PathBuf,
        source: io::Error,
    },
    UnsupportedOperationAction {
        kind: ActionKind,
    },
    #[cfg(test)]
    Injected {
        stage: CommitStage,
    },
}

impl CommitError {
    pub(crate) fn effect(&self) -> CommitFailureEffect {
        if self.replacement_completed() {
            CommitFailureEffect::ReplacedDurabilityUnconfirmed
        } else {
            CommitFailureEffect::PreviousStateRetained
        }
    }

    fn replacement_completed(&self) -> bool {
        if matches!(self, Self::DirectoryFlushAfterReplacement { .. }) {
            return true;
        }
        #[cfg(test)]
        {
            matches!(
                self,
                Self::Injected {
                    stage: CommitStage::FlushDirectory
                }
            )
        }
        #[cfg(not(test))]
        {
            false
        }
    }
}

impl fmt::Display for CommitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialize(error) => write!(formatter, "cannot serialize state: {error}"),
            Self::Hash(error) => write!(formatter, "cannot encode state hash: {error}"),
            Self::NonUnicodePath { path } => write!(
                formatter,
                "cannot serialize non-Unicode persisted path: {}",
                path.display()
            ),
            Self::TemporaryNameExhausted { state_directory } => write!(
                formatter,
                "could not allocate a unique temporary state file in {}",
                state_directory.display()
            ),
            Self::TemporaryCreate { path, source } => {
                write!(
                    formatter,
                    "cannot create temporary state file {}: {source}",
                    path.display()
                )
            }
            Self::TemporaryWrite { path, source } => {
                write!(
                    formatter,
                    "cannot write temporary state file {}: {source}",
                    path.display()
                )
            }
            Self::TemporaryFlush { path, source } => {
                write!(
                    formatter,
                    "cannot flush temporary state file {}: {source}",
                    path.display()
                )
            }
            Self::TemporaryValidation { path, source } => write!(
                formatter,
                "temporary state file {} failed validation: {source}",
                path.display()
            ),
            Self::StateReplacement {
                temporary_path,
                state_path,
                source,
            } => write!(
                formatter,
                "cannot atomically replace state file {} with {}: {source}",
                state_path.display(),
                temporary_path.display()
            ),
            Self::DirectoryFlushAfterReplacement {
                state_directory,
                source,
            } => write!(
                formatter,
                "state file was replaced but state directory {} could not be flushed: {source}",
                state_directory.display()
            ),
            Self::UnsupportedOperationAction { kind } => {
                write!(
                    formatter,
                    "cannot serialize unsupported operation action {kind:?}"
                )
            }
            #[cfg(test)]
            Self::Injected { stage } => {
                write!(formatter, "injected state commit failure at {stage:?}")
            }
        }
    }
}

impl std::error::Error for CommitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialize(error) => Some(error),
            Self::Hash(error) => Some(error),
            Self::TemporaryCreate { source, .. }
            | Self::TemporaryWrite { source, .. }
            | Self::TemporaryFlush { source, .. }
            | Self::StateReplacement { source, .. }
            | Self::DirectoryFlushAfterReplacement { source, .. } => Some(source),
            Self::TemporaryValidation { source, .. } => Some(source.as_ref()),
            Self::NonUnicodePath { .. }
            | Self::TemporaryNameExhausted { .. }
            | Self::UnsupportedOperationAction { .. } => None,
            #[cfg(test)]
            Self::Injected { .. } => None,
        }
    }
}

struct ExclusiveStateLock {
    file: File,
}

impl ExclusiveStateLock {
    fn acquire(path: &Path) -> Result<Self, StateRepositoryError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| StateRepositoryError::LockIo {
                path: path.to_path_buf(),
                source,
            })?;
        acquire_platform_lock(file, path)
    }
}

#[cfg(unix)]
fn acquire_platform_lock(
    file: File,
    path: &Path,
) -> Result<ExclusiveStateLock, StateRepositoryError> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(ExclusiveStateLock { file });
    }
    let source = io::Error::last_os_error();
    if source.kind() == io::ErrorKind::WouldBlock {
        Err(StateRepositoryError::LockContended {
            path: path.to_path_buf(),
        })
    } else {
        Err(StateRepositoryError::LockIo {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[cfg(unix)]
impl Drop for ExclusiveStateLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(windows)]
fn acquire_platform_lock(
    file: File,
    path: &Path,
) -> Result<ExclusiveStateLock, StateRepositoryError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, GetLastError};
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped = OVERLAPPED::default();
    let result = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if result != 0 {
        return Ok(ExclusiveStateLock { file });
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_LOCK_VIOLATION {
        Err(StateRepositoryError::LockContended {
            path: path.to_path_buf(),
        })
    } else {
        Err(StateRepositoryError::LockIo {
            path: path.to_path_buf(),
            source: io::Error::from_raw_os_error(error as i32),
        })
    }
}

#[cfg(windows)]
impl Drop for ExclusiveStateLock {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
        use windows_sys::Win32::System::IO::OVERLAPPED;

        let mut overlapped = OVERLAPPED::default();
        let _ = unsafe {
            UnlockFileEx(
                self.file.as_raw_handle(),
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::domain::file_link::ResolvedFileLink;
    use crate::domain::hashes::definition_hash;
    use crate::domain::ids::FullyQualifiedResourceId;
    use crate::domain::known::KnownFileLink;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestStateDirectory {
        root: PathBuf,
    }

    impl TestStateDirectory {
        fn new() -> Self {
            let nonce = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "loadout-state-repository-test-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();
            Self { root }
        }

        fn state_path(&self) -> ResolvedPath {
            ResolvedPath::new(self.root.join("state")).unwrap()
        }

        fn repository(&self) -> StateRepository {
            StateRepository::new(self.state_path())
        }

        fn state_file(&self) -> PathBuf {
            self.root.join("state").join(STATE_FILE_NAME)
        }

        fn state_directory(&self) -> PathBuf {
            self.root.join("state")
        }
    }

    impl Drop for TestStateDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn path(root: &str, leaf: &str) -> ResolvedPath {
        ResolvedPath::new(std::env::temp_dir().join(root).join(leaf)).unwrap()
    }

    fn create_action() -> PlannedAction {
        create_action_from("git/config")
    }

    fn create_action_from(source: &str) -> PlannedAction {
        PlannedAction::create_link(
            ResolvedFileLink::new(
                FullyQualifiedResourceId::parse("base/git").unwrap(),
                path("loadout-state-store", source),
                path("loadout-state-home", ".gitconfig"),
            )
            .unwrap(),
        )
    }

    fn stale_action(kind: ActionKind) -> PlannedAction {
        let previous = KnownFileLink::from_resolved(
            &ResolvedFileLink::new(
                FullyQualifiedResourceId::parse("base/git").unwrap(),
                path("loadout-state-store", "git/config"),
                path("loadout-state-home", ".gitconfig"),
            )
            .unwrap(),
        );
        match kind {
            ActionKind::RemoveLink => PlannedAction::remove_link(previous),
            ActionKind::ForgetMissing => PlannedAction::forget_missing(previous),
            _ => panic!("test helper supports only Slice 5 stale actions"),
        }
    }

    fn replace_ownership_action(source: &str) -> PlannedAction {
        let previous = KnownFileLink::from_resolved(
            &ResolvedFileLink::new(
                FullyQualifiedResourceId::parse("base/git").unwrap(),
                path("loadout-state-store", "git/config"),
                path("loadout-state-home", ".gitconfig"),
            )
            .unwrap(),
        );
        let desired = ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/git-renamed").unwrap(),
            path("loadout-state-store", source),
            path("loadout-state-home", ".gitconfig"),
        )
        .unwrap();
        PlannedAction::replace_ownership(desired, previous).unwrap()
    }

    fn relocate_action() -> PlannedAction {
        let previous = KnownFileLink::from_resolved(
            &ResolvedFileLink::new(
                FullyQualifiedResourceId::parse("base/git").unwrap(),
                path("loadout-state-store", "git/config"),
                path("loadout-state-home", ".gitconfig"),
            )
            .unwrap(),
        );
        let desired = ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/git").unwrap(),
            path("loadout-state-store", "git/config"),
            path("loadout-state-home", ".config/gitconfig"),
        )
        .unwrap();
        PlannedAction::relocate_link(desired, previous).unwrap()
    }

    fn replace_action() -> PlannedAction {
        let previous = KnownFileLink::from_resolved(
            &ResolvedFileLink::new(
                FullyQualifiedResourceId::parse("base/git").unwrap(),
                path("loadout-state-store", "git/config"),
                path("loadout-state-home", ".gitconfig"),
            )
            .unwrap(),
        );
        let desired = ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/git").unwrap(),
            path("loadout-state-store", "git/replacement"),
            path("loadout-state-home", ".gitconfig"),
        )
        .unwrap();
        PlannedAction::replace_link(desired, previous).unwrap()
    }

    fn desired_hash() -> DesiredHash {
        DesiredHash::parse(format!("sha256:{}", "a".repeat(64))).unwrap()
    }

    fn active_status(state: &PersistedState, action_id: &ActionId) -> ActionStatus {
        state
            .active_operation()
            .unwrap()
            .action(action_id)
            .unwrap()
            .status()
    }

    #[test]
    fn multi_replacement_records_unique_siblings_and_rejects_aliased_recovery_paths() {
        let workspace = TestStateDirectory::new();
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let home = workspace.root.join("home");
        fs::create_dir(&home).unwrap();
        let previous = (0..3)
            .map(|index| {
                ResolvedFileLink::new(
                    FullyQualifiedResourceId::parse(&format!("base/r{index}")).unwrap(),
                    ResolvedPath::new(workspace.root.join("old-source")).unwrap(),
                    ResolvedPath::new(home.join(format!("target{index}"))).unwrap(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let creates = previous
            .iter()
            .cloned()
            .map(PlannedAction::create_link)
            .collect::<Vec<_>>();
        let ids = locked.begin_actions(desired_hash(), &creates).unwrap();
        for id in &ids {
            locked.mark_running(id).unwrap();
            locked.commit_succeeded(id).unwrap();
        }
        locked.close_finished_operation().unwrap();
        let replacements = previous
            .iter()
            .take(2)
            .map(|old| {
                PlannedAction::replace_link(
                    ResolvedFileLink::new(
                        old.resource_id().clone(),
                        ResolvedPath::new(workspace.root.join("new-source")).unwrap(),
                        old.target_path().clone(),
                    )
                    .unwrap(),
                    KnownFileLink::from_resolved(old),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let ids = locked.begin_actions(desired_hash(), &replacements).unwrap();
        let state = repository.load().unwrap();
        let operation = state.active_operation().unwrap();
        let paths = operation
            .actions()
            .map(|(_, action)| action.replacement_facts().unwrap().temporary_path().clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(paths.len(), 2);
        assert!(
            paths
                .iter()
                .all(|path| path.as_ref().parent() == Some(home.as_path()))
        );
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
        let original = fs::read(workspace.state_file()).unwrap();
        let mut document: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let shared =
            document["active_operation"]["actions"][ids[0].as_str()]["temporary_path"].clone();
        document["active_operation"]["actions"][ids[1].as_str()]["temporary_path"] = shared;
        fs::write(
            workspace.state_file(),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        assert!(repository.load().is_err());
        let mut document: serde_json::Value = serde_json::from_slice(&original).unwrap();
        document["active_operation"]["actions"][ids[0].as_str()]["temporary_path"] =
            serde_json::Value::String(
                previous[1]
                    .target_path()
                    .as_ref()
                    .to_str()
                    .unwrap()
                    .to_owned(),
            );
        fs::write(
            workspace.state_file(),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        assert!(repository.load().is_err());
        // A temporary may not alias an unrelated Known target either.
        let mut document: serde_json::Value = serde_json::from_slice(&original).unwrap();
        document["active_operation"]["actions"][ids[0].as_str()]["temporary_path"] =
            serde_json::Value::String(
                previous[2]
                    .target_path()
                    .as_ref()
                    .to_str()
                    .unwrap()
                    .to_owned(),
            );
        fs::write(
            workspace.state_file(),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        assert!(repository.load().is_err());
    }

    #[test]
    fn multi_action_progress_is_atomic_and_ids_do_not_define_execution_order() {
        let workspace = TestStateDirectory::new();
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let actions = (0..12)
            .map(|index| {
                PlannedAction::create_link(
                    ResolvedFileLink::new(
                        FullyQualifiedResourceId::parse(&format!("base/r{index}")).unwrap(),
                        path("loadout-state-store", "source"),
                        path("loadout-state-home", &format!("target-{index}")),
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let ids = locked.begin_actions(desired_hash(), &actions).unwrap();
        let persisted = repository.load().unwrap();
        assert_eq!(persisted.active_operation().unwrap().actions().len(), 12);
        for (id, action) in ids.iter().zip(&actions) {
            let recorded = persisted.active_operation().unwrap().action(id).unwrap();
            assert_eq!(recorded.resource_id(), action.resource_id());
            assert_eq!(recorded.status(), ActionStatus::Pending);
        }
        locked.mark_running(&ids[0]).unwrap();
        locked.commit_succeeded(&ids[0]).unwrap();
        locked.mark_running(&ids[1]).unwrap();
        locked.fail_next_commit_at(CommitStage::ReplaceState);
        assert!(locked.commit_succeeded(&ids[1]).is_err());
        let persisted = repository.load().unwrap();
        assert_eq!(persisted.known().resources().len(), 1);
        assert_eq!(active_status(&persisted, &ids[0]), ActionStatus::Succeeded);
        assert_eq!(active_status(&persisted, &ids[1]), ActionStatus::Running);
        assert_eq!(active_status(&persisted, &ids[2]), ActionStatus::Pending);
        assert!(locked.close_finished_operation().is_err());
    }

    #[test]
    fn missing_state_loads_empty_without_creating_control_files() {
        let directory = TestStateDirectory::new();
        let repository = directory.repository();

        let state = repository.load().unwrap();

        assert_eq!(state, PersistedState::empty());
        assert!(!directory.state_directory().exists());
    }

    #[test]
    fn state_schema_rejects_corrupt_unknown_and_noncanonical_control_data() {
        let directory = TestStateDirectory::new();
        fs::create_dir(directory.state_directory()).unwrap();
        let repository = directory.repository();

        for json in [
            b"not JSON".as_slice(),
            br#"{"schema_version":2,"resources":{},"active_operation":null}"#,
            br#"{"schema_version":1,"resources":{},"active_operation":null,"extra":true}"#,
            br#"{"schema_version":1,"resources":{"base/git":{"definition_hash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","file_link":{"source_path":"/tmp/source","target_path":"/tmp/./target","link_target":"/tmp/source"}}},"active_operation":null}"#,
        ] {
            fs::write(directory.state_file(), json).unwrap();
            assert!(repository.load().is_err(), "{json:?} must be rejected");
        }
    }

    #[test]
    fn exclusive_lock_is_nonblocking_and_released_with_its_session() {
        let directory = TestStateDirectory::new();
        let repository = directory.repository();
        let first = repository.acquire_exclusive().unwrap();

        assert!(matches!(
            repository.acquire_exclusive(),
            Err(StateRepositoryError::LockContended { .. })
        ));
        drop(first);

        repository.acquire_exclusive().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn state_write_preflight_rejects_a_read_only_state_directory_before_progress_is_recorded() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TestStateDirectory::new();
        let repository = directory.repository();
        drop(repository.acquire_exclusive().unwrap());
        fs::set_permissions(
            directory.state_directory(),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();

        let mut locked = repository.acquire_exclusive().unwrap();
        let result = locked.preflight_writable();
        fs::set_permissions(
            directory.state_directory(),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        assert!(matches!(
            result,
            Err(StateRepositoryError::StateWritePreflight { .. })
        ));
        assert!(locked.state().active_operation().is_none());
        assert!(!directory.state_file().exists());
    }

    #[test]
    fn operation_progress_writes_pending_then_running_then_known_and_succeeded() {
        let directory = TestStateDirectory::new();
        let repository = directory.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(), &create_action())
            .unwrap();

        assert_eq!(
            active_status(locked.state(), &action_id),
            ActionStatus::Pending
        );
        assert!(locked.state().known().resources().next().is_none());
        assert_eq!(
            active_status(&repository.load().unwrap(), &action_id),
            ActionStatus::Pending
        );
        let pending_json: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.state_file()).unwrap()).unwrap();
        let recorded = &pending_json["active_operation"]["actions"]["a1"];
        assert_eq!(recorded["kind"], "create_link");
        assert_eq!(recorded["resource_id"], "base/git");
        assert_eq!(recorded["precondition"]["target"], "missing");
        assert_eq!(recorded["postcondition"]["target"], "expected_link");
        assert_eq!(recorded["status"], "pending");

        locked.mark_running(&action_id).unwrap();
        assert_eq!(
            active_status(locked.state(), &action_id),
            ActionStatus::Running
        );
        assert!(locked.state().known().resources().next().is_none());

        locked.commit_create_succeeded(&action_id).unwrap();
        assert_eq!(
            active_status(locked.state(), &action_id),
            ActionStatus::Succeeded
        );
        assert_eq!(locked.state().known().resources().len(), 1);

        locked.close_finished_operation().unwrap();
        assert!(locked.state().active_operation().is_none());

        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.state_file()).unwrap()).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert!(
            json["resources"]["base/git"]["definition_hash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }

    #[test]
    fn stale_action_progress_keeps_known_until_its_verified_succeeded_commit() {
        for (kind, expected_precondition) in [
            (ActionKind::RemoveLink, "expected_link"),
            (ActionKind::ForgetMissing, "missing"),
        ] {
            let directory = TestStateDirectory::new();
            let repository = directory.repository();
            let mut locked = repository.acquire_exclusive().unwrap();

            let create_id = locked
                .begin_create_operation(desired_hash(), &create_action())
                .unwrap();
            locked.mark_running(&create_id).unwrap();
            locked.commit_create_succeeded(&create_id).unwrap();
            locked.close_finished_operation().unwrap();

            let action_id = locked
                .begin_operation(desired_hash(), &stale_action(kind))
                .unwrap();
            let pending_json: serde_json::Value =
                serde_json::from_slice(&fs::read(directory.state_file()).unwrap()).unwrap();
            let recorded = &pending_json["active_operation"]["actions"]["a1"];
            assert_eq!(
                recorded["kind"],
                match kind {
                    ActionKind::RemoveLink => "remove_link",
                    ActionKind::ForgetMissing => "forget_missing",
                    _ => unreachable!(),
                }
            );
            assert_eq!(recorded["precondition"]["target"], expected_precondition);
            assert_eq!(recorded["postcondition"]["target"], "missing");
            assert_eq!(
                active_status(locked.state(), &action_id),
                ActionStatus::Pending
            );
            assert_eq!(locked.state().known().resources().len(), 1);

            locked.mark_running(&action_id).unwrap();
            assert_eq!(locked.state().known().resources().len(), 1);
            locked.commit_succeeded(&action_id).unwrap();
            assert_eq!(
                active_status(locked.state(), &action_id),
                ActionStatus::Succeeded
            );
            assert!(locked.state().known().resources().next().is_none());
            locked.close_finished_operation().unwrap();
            assert!(locked.state().active_operation().is_none());
        }
    }

    #[test]
    fn ownership_handoff_persists_old_and_new_identity_facts_and_commits_them_together() {
        for (source, expects_temporary) in [("git/config", false), ("git/replacement", true)] {
            let directory = TestStateDirectory::new();
            let repository = directory.repository();
            let mut locked = repository.acquire_exclusive().unwrap();

            let create_id = locked
                .begin_create_operation(desired_hash(), &create_action())
                .unwrap();
            locked.mark_running(&create_id).unwrap();
            locked.commit_create_succeeded(&create_id).unwrap();
            locked.close_finished_operation().unwrap();

            let action_id = locked
                .begin_operation(desired_hash(), &replace_ownership_action(source))
                .unwrap();
            let pending: serde_json::Value =
                serde_json::from_slice(&fs::read(directory.state_file()).unwrap()).unwrap();
            let action = &pending["active_operation"]["actions"]["a1"];
            assert_eq!(action["kind"], "replace_ownership");
            assert_eq!(action["old_resource_id"], "base/git");
            assert_eq!(action["resource_id"], "base/git-renamed");
            assert_eq!(action["temporary_path"].is_null(), !expects_temporary);

            locked.mark_running(&action_id).unwrap();
            drop(locked);
            let loaded = repository.load().unwrap();
            let recorded = loaded
                .active_operation()
                .unwrap()
                .action(&action_id)
                .unwrap();
            assert_eq!(recorded.status(), ActionStatus::Running);
            assert_eq!(
                recorded.replaced_resource_id().unwrap().as_str(),
                "base/git"
            );
            assert_eq!(recorded.replacement_facts().is_some(), expects_temporary);

            let mut locked = repository.acquire_exclusive().unwrap();
            locked.commit_succeeded(&action_id).unwrap();
            assert!(
                locked
                    .state()
                    .known()
                    .get(&FullyQualifiedResourceId::parse("base/git").unwrap())
                    .is_none()
            );
            assert!(
                locked
                    .state()
                    .known()
                    .get(&FullyQualifiedResourceId::parse("base/git-renamed").unwrap())
                    .is_some()
            );
            locked.close_finished_operation().unwrap();
        }
    }

    #[test]
    fn unfinished_ownership_handoff_with_an_existing_destination_is_rejected_as_corrupt_state() {
        let directory = TestStateDirectory::new();
        let repository = directory.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let create_id = locked
            .begin_create_operation(desired_hash(), &create_action())
            .unwrap();
        locked.mark_running(&create_id).unwrap();
        locked.commit_create_succeeded(&create_id).unwrap();
        locked.close_finished_operation().unwrap();
        let action_id = locked
            .begin_operation(desired_hash(), &replace_ownership_action("git/config"))
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        drop(locked);

        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.state_file()).unwrap()).unwrap();
        let destination = ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/git-renamed").unwrap(),
            path("loadout-state-store", "git/other"),
            path("loadout-state-home", ".other"),
        )
        .unwrap();
        document["resources"]["base/git-renamed"] = serde_json::json!({
            "definition_hash": definition_hash(&destination).unwrap().as_str(),
            "file_link": {
                "source_path": destination.source_path().as_ref(),
                "target_path": destination.target_path().as_ref(),
                "link_target": destination.link_target().as_path().as_ref(),
            }
        });
        fs::write(
            directory.state_file(),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            repository.load(),
            Err(StateRepositoryError::InvalidState {
                source: StateDecodeError::ActiveActionKnownMismatch { .. },
                ..
            })
        ));
    }

    #[test]
    fn relocation_persists_both_target_facts_and_updates_known_only_after_success() {
        let directory = TestStateDirectory::new();
        let repository = directory.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let create_id = locked
            .begin_create_operation(desired_hash(), &create_action())
            .unwrap();
        locked.mark_running(&create_id).unwrap();
        locked.commit_create_succeeded(&create_id).unwrap();
        locked.close_finished_operation().unwrap();

        let action_id = locked
            .begin_operation(desired_hash(), &relocate_action())
            .unwrap();
        let pending: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.state_file()).unwrap()).unwrap();
        let action = &pending["active_operation"]["actions"]["a1"];
        assert_eq!(action["kind"], "relocate_link");
        assert_eq!(
            action["old_target_path"].as_str(),
            path("loadout-state-home", ".gitconfig").as_ref().to_str()
        );
        assert_eq!(
            action["target_path"].as_str(),
            path("loadout-state-home", ".config/gitconfig")
                .as_ref()
                .to_str()
        );
        assert_eq!(action["precondition"]["target"], "expected_link");
        assert_eq!(action["postcondition"]["target"], "expected_link");

        locked.mark_running(&action_id).unwrap();
        drop(locked);
        let loaded = repository.load().unwrap();
        let facts = loaded
            .active_operation()
            .unwrap()
            .action(&action_id)
            .unwrap()
            .relocation_facts()
            .unwrap();
        assert_eq!(
            facts.old_target_path(),
            &path("loadout-state-home", ".gitconfig")
        );
        assert_eq!(
            facts.new_target_path(),
            &path("loadout-state-home", ".config/gitconfig")
        );

        let mut locked = repository.acquire_exclusive().unwrap();
        locked.commit_succeeded(&action_id).unwrap();
        let known = locked
            .state()
            .known()
            .get(&FullyQualifiedResourceId::parse("base/git").unwrap())
            .unwrap();
        assert_eq!(
            known.target_path(),
            &path("loadout-state-home", ".config/gitconfig")
        );
    }

    #[test]
    fn active_stale_action_without_its_required_known_fact_is_rejected_as_corrupt_state() {
        for kind in [ActionKind::RemoveLink, ActionKind::ForgetMissing] {
            let directory = TestStateDirectory::new();
            let repository = directory.repository();
            let mut locked = repository.acquire_exclusive().unwrap();

            let create_id = locked
                .begin_create_operation(desired_hash(), &create_action())
                .unwrap();
            locked.mark_running(&create_id).unwrap();
            locked.commit_create_succeeded(&create_id).unwrap();
            locked.close_finished_operation().unwrap();
            let action_id = locked
                .begin_operation(desired_hash(), &stale_action(kind))
                .unwrap();
            locked.mark_running(&action_id).unwrap();
            drop(locked);

            let mut document: serde_json::Value =
                serde_json::from_slice(&fs::read(directory.state_file()).unwrap()).unwrap();
            document["resources"] = serde_json::json!({});
            fs::write(
                directory.state_file(),
                serde_json::to_vec(&document).unwrap(),
            )
            .unwrap();

            assert!(matches!(
                repository.load(),
                Err(StateRepositoryError::InvalidState {
                    source: StateDecodeError::ActiveActionKnownMismatch { .. },
                    ..
                })
            ));
        }
    }

    #[test]
    fn unfinished_replace_link_without_its_exact_old_known_fact_is_rejected_as_corrupt_state() {
        for status in [
            ActionStatus::Pending,
            ActionStatus::Running,
            ActionStatus::Failed,
            ActionStatus::Skipped,
            ActionStatus::Uncertain,
        ] {
            let directory = TestStateDirectory::new();
            let repository = directory.repository();
            let mut locked = repository.acquire_exclusive().unwrap();
            let create_id = locked
                .begin_create_operation(desired_hash(), &create_action())
                .unwrap();
            locked.mark_running(&create_id).unwrap();
            locked.commit_create_succeeded(&create_id).unwrap();
            locked.close_finished_operation().unwrap();

            let action_id = locked
                .begin_operation(desired_hash(), &replace_action())
                .unwrap();
            match status {
                ActionStatus::Pending => {}
                ActionStatus::Running => locked.mark_running(&action_id).unwrap(),
                ActionStatus::Failed | ActionStatus::Uncertain => {
                    locked.mark_running(&action_id).unwrap();
                    locked.mark_without_known(&action_id, status).unwrap();
                }
                ActionStatus::Skipped => locked
                    .mark_without_known(&action_id, ActionStatus::Skipped)
                    .unwrap(),
                ActionStatus::Succeeded => unreachable!(),
            }
            drop(locked);

            let document: serde_json::Value =
                serde_json::from_slice(&fs::read(directory.state_file()).unwrap()).unwrap();
            for changed in [false, true] {
                let mut corrupt = document.clone();
                corrupt["resources"] = if changed {
                    let replacement = ResolvedFileLink::new(
                        FullyQualifiedResourceId::parse("base/git").unwrap(),
                        path("loadout-state-store", "git/other"),
                        path("loadout-state-home", ".gitconfig"),
                    )
                    .unwrap();
                    serde_json::json!({
                        "base/git": {
                            "definition_hash": definition_hash(&replacement).unwrap().as_str(),
                            "file_link": {
                                "source_path": replacement.source_path().as_ref(),
                                "target_path": replacement.target_path().as_ref(),
                                "link_target": replacement.link_target().as_path().as_ref(),
                            }
                        }
                    })
                } else {
                    serde_json::json!({})
                };
                fs::write(
                    directory.state_file(),
                    serde_json::to_vec(&corrupt).unwrap(),
                )
                .unwrap();

                assert!(matches!(
                    repository.load(),
                    Err(StateRepositoryError::InvalidState {
                        source: StateDecodeError::ActiveActionKnownMismatch { .. },
                        ..
                    })
                ));
            }
        }
    }

    #[test]
    fn an_uncertain_result_never_updates_known_and_keeps_its_operation_open() {
        let directory = TestStateDirectory::new();
        let repository = directory.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(), &create_action())
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        locked
            .mark_without_known(&action_id, ActionStatus::Uncertain)
            .unwrap();

        assert!(locked.state().known().resources().next().is_none());
        assert!(matches!(
            locked.close_finished_operation(),
            Err(StateRepositoryError::OperationNotCloseable)
        ));
    }

    #[test]
    fn create_success_upserts_a_known_identity_when_an_expected_link_was_missing() {
        let directory = TestStateDirectory::new();
        let repository = directory.repository();
        let mut locked = repository.acquire_exclusive().unwrap();

        let first = locked
            .begin_create_operation(desired_hash(), &create_action())
            .unwrap();
        locked.mark_running(&first).unwrap();
        locked.commit_create_succeeded(&first).unwrap();
        locked.close_finished_operation().unwrap();

        let recreated = locked
            .begin_create_operation(desired_hash(), &create_action_from("git/next"))
            .unwrap();
        locked.mark_running(&recreated).unwrap();
        locked.commit_create_succeeded(&recreated).unwrap();

        let known = locked
            .state()
            .known()
            .get(&FullyQualifiedResourceId::parse("base/git").unwrap())
            .unwrap();
        assert_eq!(
            known.source_path(),
            &path("loadout-state-store", "git/next")
        );
        assert_eq!(locked.state().known().resources().len(), 1);
    }

    #[test]
    fn a_succeeded_action_without_its_atomic_known_update_is_rejected_as_corrupt_state() {
        let directory = TestStateDirectory::new();
        let repository = directory.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(), &create_action())
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        locked.commit_create_succeeded(&action_id).unwrap();

        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.state_file()).unwrap()).unwrap();
        document["resources"] = serde_json::json!({});
        fs::write(
            directory.state_file(),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            repository.load(),
            Err(StateRepositoryError::InvalidState {
                source: StateDecodeError::SucceededActionKnownMismatch { .. },
                ..
            })
        ));
    }

    #[test]
    fn pre_replacement_commit_failures_retain_the_previous_valid_state() {
        for stage in [
            CommitStage::CreateTemporary,
            CommitStage::WriteTemporary,
            CommitStage::FlushTemporary,
            CommitStage::ReopenAndValidate,
            CommitStage::ReplaceState,
        ] {
            let directory = TestStateDirectory::new();
            let repository = directory.repository();
            let mut locked = repository.acquire_exclusive().unwrap();
            let action_id = locked
                .begin_create_operation(desired_hash(), &create_action())
                .unwrap();
            let before = repository.load().unwrap();

            locked.fail_next_commit_at(stage);
            assert!(matches!(
                locked.mark_running(&action_id),
                Err(StateRepositoryError::Commit(CommitError::Injected { stage: actual })) if actual == stage
            ));

            assert_eq!(repository.load().unwrap(), before);
            // A failed commit deliberately leaves its temporary path alone: an external actor could have replaced it after creation, so the repository cannot prove that removing it is safe.
        }
    }

    #[test]
    fn directory_flush_failure_leaves_a_complete_replaced_state_not_partial_json() {
        let directory = TestStateDirectory::new();
        let repository = directory.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(), &create_action())
            .unwrap();

        locked.fail_next_commit_at(CommitStage::FlushDirectory);
        assert!(matches!(
            locked.mark_running(&action_id),
            Err(StateRepositoryError::Commit(CommitError::Injected {
                stage: CommitStage::FlushDirectory
            }))
        ));

        assert_eq!(
            active_status(locked.state(), &action_id),
            ActionStatus::Running
        );
        assert_eq!(
            active_status(&repository.load().unwrap(), &action_id),
            ActionStatus::Running
        );
    }
}
