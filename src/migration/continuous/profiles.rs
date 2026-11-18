//! Profile-specific validation and in-memory route composition.

use super::super::{schema_3_to_4, schema_4_to_5};
use super::{PlanError, PlannedDocument, Route, routes, select_routes};
use crate::declaration::environment_config::EnvironmentConfig;
use crate::declaration::profile::{
    FileKind, FileOperation, ProfileDeclaration, ValueOrReference, parse_kind, parse_operation,
};
use crate::domain::ids::{ProfileId, ResourceId, StoreId};
use crate::domain::paths::SourceRelativePath;
use crate::resolver::{ResolverContext, validate_migration_inventory};
use serde_yaml::Value as YamlValue;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) mod publication;

pub(super) use publication::publish;

pub(super) fn plan_document(
    path: PathBuf,
    source: Vec<u8>,
    state_generation: Option<u32>,
    target: u32,
    registry: &[Route],
    environment: &EnvironmentConfig,
) -> Result<(PlannedDocument, ProfileId, Vec<ProfileId>), PlanError> {
    let text = std::str::from_utf8(&source)
        .map_err(|_| PlanError::Invalid(format!("profile {} is not UTF-8", path.display())))?;
    let from = profile_version(text)?;
    let (id, referenced) = profile_facts(from, text)?;
    validate_store_references(text, environment)?;
    if let Some(state_generation) = state_generation {
        if from > state_generation {
            prove_older_compatibility(text, from, state_generation)?;
        }
    }
    let route = select_routes(registry, from, target)?;
    let mut candidate = text.to_owned();
    for edge in &route {
        candidate = (edge.profile)(&candidate).map_err(PlanError::Invalid)?;
        profile_facts(edge.to, &candidate)?;
    }
    Ok((
        PlannedDocument {
            path,
            source_generation: from,
            target_generation: target,
            route: route.iter().map(|edge| (edge.from, edge.to)).collect(),
            source,
            candidate: candidate.into_bytes(),
        },
        id,
        referenced,
    ))
}

fn validate_store_references(
    source: &str,
    environment: &EnvironmentConfig,
) -> Result<(), PlanError> {
    let document: YamlValue =
        serde_yaml::from_str(source).map_err(|error| PlanError::Invalid(error.to_string()))?;
    let resources = document
        .as_mapping()
        .and_then(|mapping| mapping.get(YamlValue::String("resources".to_owned())))
        .and_then(YamlValue::as_mapping)
        .ok_or_else(|| PlanError::Invalid("profile has no resource mapping".to_owned()))?;
    for resource in resources.values() {
        let store = resource
            .as_mapping()
            .and_then(|mapping| mapping.get(YamlValue::String("properties".to_owned())))
            .and_then(YamlValue::as_mapping)
            .and_then(|mapping| mapping.get(YamlValue::String("source".to_owned())))
            .and_then(YamlValue::as_mapping)
            .and_then(|mapping| mapping.get(YamlValue::String("store".to_owned())))
            .and_then(YamlValue::as_str)
            .ok_or_else(|| PlanError::Invalid("profile has no source store".to_owned()))?;
        if environment.store(store).is_none() {
            return Err(PlanError::Invalid(format!(
                "profile refers to undiscovered store {store:?}"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_completed_inventory(
    documents: &[PlannedDocument],
    target: u32,
    default_profile: Option<&str>,
    context: &ResolverContext,
) -> Result<(), PlanError> {
    let mut declarations = BTreeMap::<ProfileId, ProfileDeclaration>::new();
    let mut resolved_declarations = BTreeMap::new();
    let mut source_generations = BTreeMap::new();
    for document in documents {
        let candidate = std::str::from_utf8(&document.candidate)
            .map_err(|_| PlanError::Invalid("converted profile is not UTF-8".to_owned()))?;
        let current = if target == 4 {
            schema_4_to_5::convert_profile_continuous(candidate)
                .map_err(|error| PlanError::Invalid(error.to_string()))?
        } else {
            candidate.to_owned()
        };
        let declaration = ProfileDeclaration::parse(&current)
            .map_err(|error| PlanError::Invalid(error.to_string()))?;
        let id = ProfileId::parse(declaration.id().to_owned())
            .map_err(|error| PlanError::Invalid(error.to_string()))?;
        resolved_declarations.insert(id.clone(), (declaration.clone(), document.path.clone()));
        source_generations.insert(id.clone(), document.source_generation);
        declarations.insert(id, declaration);
    }
    for (id, declaration) in &declarations {
        if default_profile == Some(id.as_str()) && !declaration.parameters().is_empty() {
            return Err(PlanError::Invalid(format!(
                "default profile {} declares parameters",
                id.as_str()
            )));
        }
        let mut aliases = BTreeMap::<String, (String, BTreeMap<String, ValueOrReference>)>::new();
        for include in declaration.includes() {
            let included_id = ProfileId::parse(include.id().to_owned())
                .map_err(|error| PlanError::Invalid(error.to_string()))?;
            let included = &declarations[&included_id];
            if source_generations[id] == 4
                && include.as_name().is_none()
                && !included.parameters().is_empty()
            {
                return Err(PlanError::Invalid(format!(
                    "schema-4 parameterized profile {} requires an instance alias",
                    included_id.as_str()
                )));
            }
            let alias = include.as_name().unwrap_or(include.id()).to_owned();
            for (parameter, expected) in included.parameters() {
                let binding = include.bindings().get(parameter).ok_or_else(|| {
                    PlanError::Invalid(format!(
                        "include of {} omits binding {parameter}",
                        included_id.as_str()
                    ))
                })?;
                let valid = match binding {
                    ValueOrReference::Value(value) => value.matches(*expected),
                    ValueOrReference::Reference(reference) => {
                        declaration.parameters().get(reference) == Some(expected)
                    }
                };
                if !valid {
                    return Err(PlanError::Invalid(format!(
                        "include of {} has invalid binding {parameter}",
                        included_id.as_str()
                    )));
                }
            }
            if include
                .bindings()
                .keys()
                .any(|parameter| !included.parameters().contains_key(parameter))
            {
                return Err(PlanError::Invalid(format!(
                    "include of {} has an unknown binding",
                    included_id.as_str()
                )));
            }
            let binding_facts = (include.id().to_owned(), include.bindings().clone());
            if aliases
                .insert(alias.clone(), binding_facts.clone())
                .is_some_and(|previous| previous != binding_facts)
            {
                return Err(PlanError::Invalid(format!(
                    "conflicting instance alias {alias}"
                )));
            }
        }
        for (_, resource) in declaration.resources() {
            for value in [
                resource.properties().kind(),
                resource.properties().operation(),
                resource.properties().target(),
                resource.properties().source().path(),
            ] {
                if let ValueOrReference::Reference(parameter) = value {
                    if !matches!(
                        declaration.parameters().get(parameter),
                        Some(crate::declaration::profile::ParameterType::String)
                    ) {
                        return Err(PlanError::Invalid(format!(
                            "resource refers to an undeclared or non-string parameter {parameter}"
                        )));
                    }
                }
            }
            if let Some(stage) = resource.stage() {
                if !declaration.stages().any(|(id, _)| id == stage) {
                    return Err(PlanError::Invalid(format!(
                        "resource refers to undeclared local stage {stage}"
                    )));
                }
            }
            for reference in resource.depends_on() {
                if !reference.contains('/')
                    && !declaration.resources().any(|(id, _)| id == reference)
                {
                    return Err(PlanError::Invalid(format!(
                        "resource depends on undeclared local resource {reference}"
                    )));
                }
            }
        }
        for (_, stage) in declaration.stages() {
            for reference in stage.after() {
                if !reference.contains('/') && !declaration.stages().any(|(id, _)| id == reference)
                {
                    return Err(PlanError::Invalid(format!(
                        "stage follows undeclared local stage {reference}"
                    )));
                }
            }
        }
    }
    validate_migration_inventory(context, &resolved_declarations)
        .map_err(|error| PlanError::Invalid(error.to_string()))?;
    Ok(())
}

pub(super) fn profile_version(source: &str) -> Result<u32, PlanError> {
    let document: YamlValue =
        serde_yaml::from_str(source).map_err(|error| PlanError::Invalid(error.to_string()))?;
    document
        .as_mapping()
        .and_then(|mapping| mapping.get(YamlValue::String("schema_version".to_owned())))
        .and_then(YamlValue::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| PlanError::Invalid("profile has no valid schema_version".to_owned()))
}

pub(super) fn profile_facts(
    version: u32,
    source: &str,
) -> Result<(ProfileId, Vec<ProfileId>), PlanError> {
    match version {
        3 => schema_3_to_4::profile_facts(source)
            .map_err(|error| PlanError::Invalid(error.to_string())),
        4 => schema_4_to_5::profile_facts_continuous(source)
            .map_err(|error| PlanError::Invalid(error.to_string())),
        5 => {
            let profile = ProfileDeclaration::parse(source)
                .map_err(|error| PlanError::Invalid(error.to_string()))?;
            let id = ProfileId::parse(profile.id().to_owned())
                .map_err(|error| PlanError::Invalid(error.to_string()))?;
            for parameter in profile.parameters().keys() {
                ProfileId::parse(parameter.to_owned())
                    .map_err(|error| PlanError::Invalid(error.to_string()))?;
            }
            for (resource_id, resource) in profile.resources() {
                ResourceId::parse(resource_id.to_owned())
                    .map_err(|error| PlanError::Invalid(error.to_string()))?;
                let properties = resource.properties();
                let kind = literal_enum(properties.kind(), parse_kind)?;
                let operation = literal_enum(properties.operation(), parse_operation)?;
                if kind == Some(FileKind::Directory) && operation == Some(FileOperation::Copy) {
                    return Err(PlanError::Invalid(
                        "directory/copy is not supported".to_owned(),
                    ));
                }
                if let ValueOrReference::Value(value) = properties.target() {
                    if value.as_string().is_none() {
                        return Err(PlanError::Invalid(
                            "profile target must be a string".to_owned(),
                        ));
                    }
                }
                StoreId::parse(properties.source().store().to_owned())
                    .map_err(|error| PlanError::Invalid(error.to_string()))?;
                match properties.source().path() {
                    ValueOrReference::Value(value) => {
                        let path = value.as_string().ok_or_else(|| {
                            PlanError::Invalid("profile source.path must be a string".to_owned())
                        })?;
                        SourceRelativePath::parse(path)
                            .map_err(|error| PlanError::Invalid(format!("{error:?}")))?;
                    }
                    ValueOrReference::Reference(_) => {}
                }
            }
            for (stage, _) in profile.stages() {
                ResourceId::parse(stage.to_owned())
                    .map_err(|error| PlanError::Invalid(error.to_string()))?;
            }
            let includes = profile
                .includes()
                .iter()
                .map(|include| {
                    if let Some(alias) = include.as_name() {
                        ProfileId::parse(alias.to_owned())
                            .map_err(|error| PlanError::Invalid(error.to_string()))?;
                    }
                    for binding in include.bindings().keys() {
                        ProfileId::parse(binding.to_owned())
                            .map_err(|error| PlanError::Invalid(error.to_string()))?;
                    }
                    ProfileId::parse(include.id().to_owned())
                        .map_err(|error| PlanError::Invalid(error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((id, includes))
        }
        _ => Err(PlanError::UnsupportedGeneration {
            from: version,
            target: 5,
        }),
    }
}

fn literal_enum<T: Copy>(
    value: &ValueOrReference,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Result<Option<T>, PlanError> {
    match value {
        ValueOrReference::Value(value) => value
            .as_string()
            .and_then(parse)
            .map(Some)
            .ok_or_else(|| PlanError::Invalid("invalid closed profile property".to_owned())),
        ValueOrReference::Reference(_) => Ok(None),
    }
}

pub(super) fn prove_older_compatibility(
    source: &str,
    from: u32,
    older: u32,
) -> Result<(), PlanError> {
    let mut lowered = source.to_owned();
    for generation in (older + 1..=from).rev() {
        lowered = schema_4_to_5::replace_root_schema_version(
            &lowered,
            char::from_digit(generation, 10).expect("supported generation"),
            char::from_digit(generation - 1, 10).expect("supported generation"),
        )
        .map_err(|error| PlanError::Invalid(error.to_string()))?;
        profile_facts(generation - 1, &lowered)?;
    }
    let registry = routes();
    let mut restored = lowered;
    for edge in select_routes(&registry, older, from)? {
        restored = (edge.profile)(&restored).map_err(PlanError::Invalid)?;
        profile_facts(edge.to, &restored)?;
    }
    if restored != source {
        return Err(PlanError::Invalid(
            "newer profile cannot be reproduced from the older generation".to_owned(),
        ));
    }
    Ok(())
}
