use crate::application::queries::{
    ConfigurationReport, ConfigurationValue, DesiredResourcesReport, DiffReport,
    KnownResourcesReport, PlanReport, ProfileListReport, ProfileShowReport, QueryError,
    StatusActual, StatusDesired, StatusRelationship, StatusReport, StatusUnavailable,
    ValidationReport,
};
use crate::domain::{
    actual::TargetObservation,
    diagnostic::Diagnostic,
    plan::{ActionKind, ActionReason, Plan},
};
use crate::state::operation::ActionStatus;
use std::{
    io::{self, Write},
    path::Path,
};

pub(super) fn action_name(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::CreateLink => "create_link",
        ActionKind::ReplaceLink => "replace_link",
        ActionKind::RelocateLink => "relocate_link",
        ActionKind::ReplaceOwnership => "replace_ownership",
        ActionKind::RemoveLink => "remove_link",
        ActionKind::ForgetMissing => "forget_missing",
        ActionKind::CreateCopy => "create_copy",
        ActionKind::ReplaceCopy => "replace_copy",
        ActionKind::RelocateCopy => "relocate_copy",
        ActionKind::RemoveCopy => "remove_copy",
        ActionKind::ReplaceEffect => "replace_effect",
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

pub(super) fn profile_list(out: &mut impl Write, report: &ProfileListReport) -> io::Result<()> {
    writeln!(out, "Profiles: {}", report.profiles.len())?;
    for profile in &report.profiles {
        writeln!(out, "{}", profile.id())?;
    }
    Ok(())
}

pub(super) fn profile_show(out: &mut impl Write, report: &ProfileShowReport) -> io::Result<()> {
    let profile = &report.profile;
    writeln!(out, "profile: {}", profile.id())?;
    for include in profile.includes() {
        writeln!(out, "include: {include}")?;
    }
    for resource in profile.resources() {
        writeln!(
            out,
            "resource {}: file link: store {}: source {}: target {}",
            resource.resource_id(),
            resource.store_id(),
            resource.source_path(),
            resource.target_path()
        )?;
    }
    Ok(())
}

pub(super) fn desired_resources(
    out: &mut impl Write,
    report: &DesiredResourcesReport,
) -> io::Result<()> {
    writeln!(
        out,
        "Desired resources for {}: {}",
        report.root_profile,
        report.resources.len()
    )?;
    for resource in &report.resources {
        render_resolved_resource(out, resource)?;
    }
    Ok(())
}

pub(super) fn desired_resource(
    out: &mut impl Write,
    root_profile: &crate::domain::ids::ProfileId,
    resource: &crate::domain::desired::ResolvedResource,
) -> io::Result<()> {
    writeln!(out, "Desired resource for {root_profile}:")?;
    render_resolved_resource(out, resource)
}

pub(super) fn known_resources(
    out: &mut impl Write,
    report: &KnownResourcesReport,
) -> io::Result<()> {
    writeln!(out, "Known resources: {}", report.resources.len())?;
    for resource in &report.resources {
        render_known_resource(out, resource)?;
    }
    Ok(())
}

pub(super) fn known_resource(
    out: &mut impl Write,
    resource: &crate::domain::known::KnownResource,
) -> io::Result<()> {
    render_known_resource(out, resource)
}

pub(super) fn status(out: &mut impl Write, report: &StatusReport) -> io::Result<()> {
    match &report.desired {
        StatusDesired::Available {
            root_profile,
            resources,
        } => {
            writeln!(out, "status profile: {root_profile}")?;
            for resource in resources {
                let relationship = match resource.relationship() {
                    StatusRelationship::DesiredOnly => "desired_only",
                    StatusRelationship::KnownOnly => "known_only",
                    StatusRelationship::DefinitionChanged => "definition_changed",
                    StatusRelationship::DefinitionsMatch => "definitions_match",
                };
                writeln!(
                    out,
                    "{}: desired-to-known: {relationship}",
                    resource.resource_id
                )?;
                match &resource.actual {
                    StatusActual::Available(actual) => {
                        let (comparison, category) = match resource.relationship() {
                            StatusRelationship::DesiredOnly => {
                                ("desired-to-actual", "desired_target_observation")
                            }
                            StatusRelationship::DefinitionsMatch
                                if matches!(
                                    actual.observation(),
                                    TargetObservation::ExpectedLink { .. }
                                ) =>
                            {
                                ("known-to-actual", "recorded_and_expected")
                            }
                            _ if matches!(
                                actual.observation(),
                                TargetObservation::ExpectedLink { .. }
                            ) =>
                            {
                                ("known-to-actual", "expected_link")
                            }
                            _ => ("known-to-actual", "drifted"),
                        };
                        writeln!(
                            out,
                            "  {comparison}: {category}: {}: {}",
                            actual.target_path(),
                            observation(actual.observation())
                        )?;
                    }
                    StatusActual::Unavailable(StatusUnavailable::InspectorInitialization) => {
                        let (comparison, category) = unavailable_status_category(resource);
                        writeln!(
                            out,
                            "  {comparison}: {category}: unavailable: inspector initialization failed"
                        )?;
                    }
                    StatusActual::Unavailable(StatusUnavailable::Observation(error)) => {
                        let (comparison, category) = unavailable_status_category(resource);
                        writeln!(out, "  {comparison}: {category}: unavailable: {error}")?;
                    }
                }
            }
        }
        StatusDesired::Unavailable(error) => {
            writeln!(out, "desired_unavailable: {}", query_error(error))?;
        }
    }
    if let Some(error) = &report.inspection_initialization_error {
        writeln!(out, "inspection unavailable: {error}")?;
    }
    if let Some(operation) = &report.active_operation {
        writeln!(out, "active_operation: {}", operation.id().as_str())?;
        for (id, action) in operation.actions() {
            if !action.status().closes_operation() {
                let status = match action.status() {
                    ActionStatus::Pending => "pending",
                    ActionStatus::Running => "running",
                    ActionStatus::Uncertain => "uncertain",
                    _ => unreachable!("only unfinished actions are rendered"),
                };
                writeln!(
                    out,
                    "  {}: {} {}: {}: {status}",
                    id.as_str(),
                    action_name(action.kind()),
                    action.resource_id(),
                    action.target_path(),
                )?;
            }
        }
    }
    Ok(())
}

fn unavailable_status_category(
    resource: &crate::application::queries::StatusResource,
) -> (&'static str, &'static str) {
    match resource.relationship() {
        StatusRelationship::DesiredOnly => ("desired-to-actual", "desired_target_observation"),
        StatusRelationship::KnownOnly
        | StatusRelationship::DefinitionChanged
        | StatusRelationship::DefinitionsMatch => ("known-to-actual", "drifted"),
    }
}

fn render_resolved_resource(
    out: &mut impl Write,
    resource: &crate::domain::desired::ResolvedResource,
) -> io::Result<()> {
    match resource {
        crate::domain::desired::ResolvedResource::FileLink(resource) => writeln!(
            out,
            "{}: file link: source {}: target {}: operation link",
            resource.resource_id(),
            resource.source_path(),
            resource.target_path()
        ),
        crate::domain::desired::ResolvedResource::FileCopy(resource) => writeln!(
            out,
            "{}: file copy: source {}: target {}: operation copy",
            resource.resource_id(),
            resource.source_path(),
            resource.target_path()
        ),
    }
}

fn render_known_resource(
    out: &mut impl Write,
    resource: &crate::domain::known::KnownResource,
) -> io::Result<()> {
    match resource {
        crate::domain::known::KnownResource::FileLink(resource) => writeln!(
            out,
            "{}: file link: source {}: target {}",
            resource.resource_id(),
            resource.source_path(),
            resource.target_path()
        ),
        crate::domain::known::KnownResource::FileCopy(resource) => writeln!(
            out,
            "{}: file copy: source {}: target {}: content {}",
            resource.resource_id(),
            resource.source_path(),
            resource.target_path(),
            resource.content_fingerprint().as_str()
        ),
    }
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
    planned(out, err, &report.plan)
}

pub(super) fn planned(out: &mut impl Write, err: &mut impl Write, plan: &Plan) -> io::Result<u8> {
    for action in plan.actions() {
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
    for diagnostic in plan.diagnostics() {
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
            Diagnostic::UnexpectedCopyTarget {
                resource_id,
                target_path,
                observation,
            } => writeln!(
                err,
                "conflict: {resource_id}: {target_path}: {observation:?}"
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
            Diagnostic::UnsupportedResourceEffect {
                resource_id,
                target_path,
                effect,
            } => writeln!(
                err,
                "blocked: {resource_id}: {target_path}: {effect} planning is not implemented"
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
    Ok(if plan.is_executable() { 0 } else { 2 })
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
        QueryError::ConfigField(field) => format!("unsupported configuration field: {field}"),
        QueryError::UnsupportedResourceEffect {
            resource_id,
            effect,
        } => format!("resource {resource_id} uses unsupported {effect} read-only rendering"),
    }
}

pub(super) fn config_path(out: &mut impl Write, path: &Path) -> io::Result<u8> {
    writeln!(out, "{}", path.display())?;
    Ok(0)
}

pub(super) fn config_list(out: &mut impl Write, report: &ConfigurationReport) -> io::Result<u8> {
    writeln!(out, "configuration: {}", report.configuration_path)?;
    match &report.default_profile {
        Some(profile) => writeln!(out, "default_profile: {profile}")?,
        None => writeln!(out, "default_profile: <unset>")?,
    }
    for (store_id, path) in &report.stores {
        writeln!(out, "stores.{store_id}.properties.path: {path}")?;
    }
    Ok(0)
}

pub(super) fn config_get(
    out: &mut impl Write,
    report: crate::application::queries::ConfigurationValueReport,
    field: &str,
) -> io::Result<u8> {
    writeln!(out, "configuration: {}", report.configuration_path)?;
    match report.value {
        ConfigurationValue::DefaultProfile(profile) => match profile {
            Some(profile) => writeln!(out, "default_profile: {profile}"),
            None => writeln!(out, "default_profile: <unset>"),
        },
        ConfigurationValue::StorePath(path) => writeln!(out, "{field}: {path}"),
    }?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::queries::StatusResource,
        domain::{
            actual::{ActualFileLink, TargetObservation},
            file_link::ResolvedFileLink,
            ids::FullyQualifiedResourceId,
            known::KnownFileLink,
            paths::ResolvedPath,
        },
    };

    #[test]
    fn status_renders_a_desired_only_unavailable_observation_without_claiming_known_drift() {
        let temporary = std::env::temp_dir();
        let resource = ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/item").unwrap(),
            ResolvedPath::new(temporary.join("loadout-render-source")).unwrap(),
            ResolvedPath::new(temporary.join("loadout-render-target")).unwrap(),
        )
        .unwrap();
        let report = StatusReport {
            active_operation: None,
            desired: StatusDesired::Available {
                root_profile: crate::domain::ids::ProfileId::parse("base").unwrap(),
                resources: vec![StatusResource {
                    resource_id: resource.resource_id().clone(),
                    desired: Some(resource),
                    known: None,
                    actual: StatusActual::Unavailable(StatusUnavailable::InspectorInitialization),
                }],
            },
            has_unavailable_observation: true,
            inspection_initialization_error: None,
        };

        let mut output = Vec::new();
        status(&mut output, &report).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("desired-to-actual: desired_target_observation: unavailable"));
        assert!(!output.contains("known-to-actual: drifted"));
    }

    #[test]
    fn status_renders_definition_changed_and_expected_known_observation_separately() {
        let temporary = std::env::temp_dir();
        let resource_id = FullyQualifiedResourceId::parse("base/item").unwrap();
        let target_path = ResolvedPath::new(temporary.join("loadout-render-target")).unwrap();
        let desired = ResolvedFileLink::new(
            resource_id.clone(),
            ResolvedPath::new(temporary.join("loadout-render-desired-source")).unwrap(),
            target_path.clone(),
        )
        .unwrap();
        let known_definition = ResolvedFileLink::new(
            resource_id.clone(),
            ResolvedPath::new(temporary.join("loadout-render-known-source")).unwrap(),
            target_path.clone(),
        )
        .unwrap();
        let known = KnownFileLink::from_resolved(&known_definition);
        let actual = ActualFileLink::new(
            target_path,
            TargetObservation::ExpectedLink {
                link_target: known.link_target().clone(),
            },
        )
        .unwrap();
        let report = StatusReport {
            active_operation: None,
            desired: StatusDesired::Available {
                root_profile: crate::domain::ids::ProfileId::parse("base").unwrap(),
                resources: vec![StatusResource {
                    resource_id,
                    desired: Some(desired),
                    known: Some(known),
                    actual: StatusActual::Available(actual),
                }],
            },
            has_unavailable_observation: false,
            inspection_initialization_error: None,
        };

        let mut output = Vec::new();
        status(&mut output, &report).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("desired-to-known: definition_changed"));
        assert!(output.contains("known-to-actual: expected_link:"));
        assert!(output.contains("expected_link"));
        assert!(!output.contains("drifted"));
    }
}
