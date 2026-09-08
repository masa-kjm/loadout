//! Confirmation and presentation only; all lifecycle decisions belong to application.
use super::render;
use crate::application::{
    apply::{self, ApplyError, ApplyFailure, ApplyFailureCause, ApplyReport, ApplyStage},
    queries::DeclarationRequest,
};
use crate::state::repository::{CommitFailureEffect, OperationOutcome};
use std::io::{self, BufRead, Write};

pub(super) fn run(
    request: &DeclarationRequest,
    yes: bool,
    dry_run: bool,
    input: &mut impl BufRead,
    interactive: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<u8> {
    if dry_run {
        return super::render_result(
            apply::dry_run(request).map(|report| render::plan(out, err, &report)),
            err,
        );
    }
    let mut confirmation_error = None;
    let result = apply::apply_request(request, |plan| {
        let confirmation = (|| -> io::Result<bool> {
            writeln!(out, "executable plan")?;
            render::planned(out, err, plan)?;
            out.flush()?;
            if yes {
                return Ok(true);
            }
            if !interactive {
                writeln!(
                    err,
                    "confirmation unavailable: non-interactive apply requires --yes"
                )?;
                return Ok(false);
            }
            write!(err, "Apply this plan? [y/N] ")?;
            err.flush()?;
            let mut response = String::new();
            input.read_line(&mut response)?;
            Ok(matches!(
                response.trim().to_ascii_lowercase().as_str(),
                "y" | "yes"
            ))
        })();
        match confirmation {
            Ok(accepted) => accepted,
            Err(error) => {
                confirmation_error = Some(error);
                false
            }
        }
    });
    if let Some(error) = confirmation_error {
        return Err(error);
    }
    match result {
        Ok(ApplyReport::Applied { committed, .. }) => {
            writeln!(
                out,
                "apply completed: {} committed actions",
                committed.len()
            )?;
            Ok(0)
        }
        Ok(ApplyReport::Blocked { plan }) => {
            writeln!(out, "blocked plan")?;
            render::planned(out, err, &plan)
        }
        Ok(ApplyReport::Declined { .. }) => {
            writeln!(err, "apply cancelled: confirmation not granted")?;
            Ok(2)
        }
        Err(failure) => {
            failure_output(err, &failure)?;
            Ok(failure_exit_code(&failure))
        }
    }
}

fn failure_exit_code(failure: &ApplyFailure) -> u8 {
    if !failure.committed.is_empty() || failure.commit_failure.is_some() {
        return 1;
    }
    match (&failure.stage, &failure.cause) {
        (_, ApplyFailureCause::Input(error)) => super::query_exit_code(error),
        (ApplyStage::Recovery, ApplyFailureCause::Lifecycle(ApplyError::RecoveryRequired)) => 2,
        (ApplyStage::Preflight, _) => 2,
        _ => 1,
    }
}

fn failure_output(err: &mut impl Write, failure: &ApplyFailure) -> io::Result<()> {
    let cause = match &failure.cause {
        ApplyFailureCause::Input(error) => render::query_error(error),
        ApplyFailureCause::Lifecycle(error) => error.to_string(),
    };
    writeln!(err, "apply failed during {:?}: {cause}", failure.stage)?;
    if let Some(action) = &failure.affected_action {
        writeln!(
            err,
            "affected action: {} {}{}",
            render::action_name(action.kind),
            action.resource_id,
            action
                .action_id
                .as_ref()
                .map(|id| format!(" ({})", id.as_str()))
                .unwrap_or_default()
        )?;
    }
    if let Some(plan) = &failure.plan {
        writeln!(
            err,
            "plan: {}; actions at failure:",
            if plan.is_executable() {
                "executable"
            } else {
                "blocked"
            }
        )?;
        // Use a separate buffer so diagnostics and actions can share stderr in order.
        let mut diagnostics = Vec::new();
        render::planned(err, &mut diagnostics, plan)?;
        err.write_all(&diagnostics)?;
    }
    writeln!(err, "committed actions: {}", failure.committed.len())?;
    for resource in &failure.committed {
        writeln!(err, "  {resource}")?;
    }
    let operation = match &failure.operation {
        None => {
            writeln!(err, "operation: unavailable (lock or state load failed)")?;
            None
        }
        Some(OperationOutcome::Absent) => {
            writeln!(err, "operation: absent")?;
            None
        }
        Some(OperationOutcome::Retained(op)) => {
            writeln!(err, "operation retained: {}", op.id().as_str())?;
            Some(op)
        }
        Some(OperationOutcome::Closed(op)) => {
            writeln!(err, "operation closed: {}", op.id().as_str())?;
            Some(op)
        }
    };
    if let Some(operation) = operation {
        for (id, action) in operation.actions() {
            writeln!(
                err,
                "  {}: {} {}: {}: recorded status {:?}",
                id.as_str(),
                render::action_name(action.kind()),
                action.resource_id(),
                action.target_path(),
                action.status()
            )?;
        }
    }
    match failure.commit_failure {
        Some(CommitFailureEffect::PreviousStateRetained) => {
            writeln!(err, "commit failed: previous state retained")?
        }
        Some(CommitFailureEffect::ReplacedDurabilityUnconfirmed) => writeln!(
            err,
            "commit failed: state replaced; durability unconfirmed (recorded status is not confirmed durable)"
        )?,
        None => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::apply::ApplyAction,
        domain::{
            file_link::LinkTarget,
            hashes::DesiredHash,
            ids::FullyQualifiedResourceId,
            paths::ResolvedPath,
            plan::{ActionKind, TargetCondition},
        },
        state::{
            operation::{ActionId, ActionStatus, OperationId, OperationRecord, RecordedAction},
            repository::{CommitError, CommitStage, StateRepositoryError},
        },
    };

    #[test]
    fn commit_failure_reports_recorded_success_separately_from_confirmed_commits() {
        let target =
            ResolvedPath::new(std::env::temp_dir().join("loadout-render-only-target")).unwrap();
        let resource = FullyQualifiedResourceId::parse("base/item").unwrap();
        let action_id = ActionId::parse("a2").unwrap();
        let record = RecordedAction::from_persisted(
            ActionKind::CreateLink,
            resource.clone(),
            target.clone(),
            TargetCondition::Missing {
                target_path: target.clone(),
            },
            TargetCondition::ExpectedLink {
                target_path: target.clone(),
                link_target: LinkTarget::new(
                    ResolvedPath::new(std::env::temp_dir().join("loadout-render-only-source"))
                        .unwrap(),
                ),
            },
            ActionStatus::Succeeded,
        )
        .unwrap();
        let operation = OperationRecord::from_actions(
            OperationId::parse("operation-render-only").unwrap(),
            DesiredHash::parse(format!("sha256:{}", "a".repeat(64))).unwrap(),
            [(action_id.clone(), record)],
        )
        .unwrap();
        for (stage, effect, needle) in [
            (
                CommitStage::ReplaceState,
                CommitFailureEffect::PreviousStateRetained,
                "previous state retained",
            ),
            (
                CommitStage::FlushDirectory,
                CommitFailureEffect::ReplacedDurabilityUnconfirmed,
                "durability unconfirmed",
            ),
        ] {
            for closed in [false, true] {
                let failure = ApplyFailure {
                    stage: if closed {
                        ApplyStage::Closure
                    } else {
                        ApplyStage::Execution
                    },
                    cause: ApplyFailureCause::Lifecycle(ApplyError::State(
                        StateRepositoryError::Commit(CommitError::Injected { stage }),
                    )),
                    affected_action: Some(ApplyAction {
                        action_id: Some(action_id.clone()),
                        resource_id: resource.clone(),
                        kind: ActionKind::CreateLink,
                    }),
                    operation: Some(if closed {
                        OperationOutcome::Closed(operation.clone())
                    } else {
                        OperationOutcome::Retained(operation.clone())
                    }),
                    commit_failure: Some(effect),
                    committed: vec![FullyQualifiedResourceId::parse("base/earlier").unwrap()],
                    plan: None,
                };
                assert_eq!(failure_exit_code(&failure), 1);
                let mut output = Vec::new();
                failure_output(&mut output, &failure).unwrap();
                let text = String::from_utf8(output).unwrap();
                for expected in [
                    needle,
                    "recorded status Succeeded",
                    "committed actions: 1",
                    "base/earlier",
                    "base/item",
                    "a2",
                    &target.to_string(),
                    if closed {
                        "operation closed"
                    } else {
                        "operation retained"
                    },
                ] {
                    assert!(text.contains(expected), "{text}");
                }
            }
        }
    }
}
