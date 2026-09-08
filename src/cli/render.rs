use crate::application::queries::{DiffReport, PlanReport, QueryError, ValidationReport};
use crate::domain::{
    actual::TargetObservation,
    diagnostic::Diagnostic,
    plan::{ActionKind, ActionReason},
};
use crate::state::operation::ActionStatus;
use std::io::{self, Write};

fn action_name(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::CreateLink => "create_link",
        ActionKind::ReplaceLink => "replace_link",
        ActionKind::RelocateLink => "relocate_link",
        ActionKind::ReplaceOwnership => "replace_ownership",
        ActionKind::RemoveLink => "remove_link",
        ActionKind::ForgetMissing => "forget_missing",
        ActionKind::Noop => "noop",
    }
}

fn reason(reason: ActionReason) -> &'static str {
    match reason {
        ActionReason::TargetMissing => "target missing",
        ActionReason::SourceChanged => "source changed",
        ActionReason::TargetChanged => "target changed",
        ActionReason::ManagedIdentityHandoff => "managed identity handoff",
        ActionReason::StaleResource => "stale resource",
        ActionReason::StaleResourceTargetMissing => "stale resource target missing",
        ActionReason::AlreadySatisfied => "already satisfied",
    }
}

fn observation(observation: &TargetObservation) -> String {
    match observation {
        TargetObservation::Missing => "missing".into(),
        TargetObservation::ExpectedLink { link_target } => {
            format!("expected_link -> {}", link_target.as_path())
        }
        TargetObservation::MatchingUnmanagedLink { link_target } => {
            format!("matching_unmanaged_link -> {}", link_target.as_path())
        }
        TargetObservation::OtherLink { link_target } => {
            format!("other_link -> {}", link_target.as_path())
        }
        TargetObservation::OtherEntry { kind } => format!("other_entry ({kind:?})"),
        TargetObservation::UnsafePath { parent_safety } => {
            format!("unsafe_path ({parent_safety:?})")
        }
    }
}

pub(super) fn validation(
    out: &mut impl Write,
    err: &mut impl Write,
    report: &ValidationReport,
) -> io::Result<u8> {
    let mut code = 0;
    for (profile, result) in &report.profiles {
        match result {
            Ok(()) => writeln!(out, "valid profile: {profile}")?,
            Err(error) => {
                writeln!(err, "invalid profile {profile}: {error}")?;
                code = if code == 1 || super::resolver_exit_code(error) == 1 {
                    1
                } else {
                    2
                };
            }
        }
    }
    if report.profiles.is_empty() {
        writeln!(out, "No profiles discovered.")?;
    }
    Ok(code)
}

pub(super) fn diff(out: &mut impl Write, report: &DiffReport) -> io::Result<u8> {
    writeln!(out, "Known resources: {}", report.resources.len())?;
    for (id, actual) in &report.resources {
        writeln!(
            out,
            "{id}: {}: {}",
            actual.target_path(),
            observation(actual.observation())
        )?;
    }
    if let Some(operation) = &report.active_operation {
        writeln!(out, "active operation: {}", operation.id().as_str())?;
        for (id, action) in operation.actions() {
            let status = match action.status() {
                ActionStatus::Pending => "pending",
                ActionStatus::Running => "running",
                ActionStatus::Uncertain => "uncertain",
                _ => continue,
            };
            writeln!(
                out,
                "  {}: {} {}: {}: {status}",
                id.as_str(),
                action_name(action.kind()),
                action.resource_id(),
                action.target_path()
            )?;
        }
    }
    Ok(0)
}

pub(super) fn plan(
    out: &mut impl Write,
    err: &mut impl Write,
    report: &PlanReport,
) -> io::Result<u8> {
    writeln!(
        out,
        "profile {}: {} plan",
        report.profile,
        if report.plan.is_executable() {
            "executable"
        } else {
            "blocked"
        }
    )?;
    for action in report.plan.actions() {
        let paths = action
            .preconditions()
            .iter()
            .map(|condition| condition.target_path().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let identity = match action.replaced_resource_id() {
            Some(old) => format!("{old} -> {}", action.resource_id()),
            None => action.resource_id().to_string(),
        };
        writeln!(
            out,
            "{} {}: {paths}: {}",
            action_name(action.kind()),
            identity,
            reason(action.reason())
        )?;
    }
    for diagnostic in report.plan.diagnostics() {
        match diagnostic {
            Diagnostic::TargetCollision {
                target_path,
                resource_ids,
            } => writeln!(
                err,
                "conflict: target {target_path} claimed by {}",
                resource_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )?,
            Diagnostic::UnexpectedTarget {
                resource_id,
                target_path,
                observation: actual,
            } => writeln!(
                err,
                "conflict: {resource_id}: {target_path}: {}",
                observation(actual)
            )?,
            Diagnostic::MissingActualObservation { target_path } => {
                writeln!(err, "blocked: missing observation for {target_path}")?
            }
            Diagnostic::UnsupportedPlatform {
                resource_id,
                action_kind,
            } => writeln!(
                err,
                "blocked: {resource_id}: unsupported {}",
                action_name(*action_kind)
            )?,
            Diagnostic::IdentityHandoffPrecondition {
                old_resource_id,
                new_resource_id,
                target_path,
                observation: actual,
            } => writeln!(
                err,
                "conflict: {old_resource_id} -> {new_resource_id}: {target_path}: {}",
                observation(actual)
            )?,
        }
    }
    Ok(if report.plan.is_executable() { 0 } else { 2 })
}

pub(super) fn query_error(error: &QueryError) -> String {
    match error {
        QueryError::ConfigurationRead { path, source } => {
            format!("cannot read configuration {path}: {source}")
        }
        QueryError::Configuration(error) => format!("invalid environment configuration: {error}"),
        QueryError::Resolution(error) => error.to_string(),
        QueryError::State(error) => error.to_string(),
        QueryError::Inspection(error) => error.to_string(),
    }
}
