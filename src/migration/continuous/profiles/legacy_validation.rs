//! Validation projection for explicit targets below the current generation.

use std::collections::{BTreeMap, BTreeSet};

use serde_yaml::{Mapping, Value};

use super::{PlanError, PlannedDocument, profile_facts};
use crate::domain::ids::ProfileId;
use crate::migration::schema_4_to_5;

struct LegacyProfile {
    value: Value,
    includes: Vec<(String, String)>,
    stages: BTreeMap<String, Vec<String>>,
    resources: BTreeMap<String, Vec<String>>,
    resource_stages: BTreeMap<String, Option<String>>,
    parameterless: bool,
}

fn key(name: &str) -> Value {
    Value::String(name.to_owned())
}

fn mapping<'a>(value: &'a Value, field: &str) -> Result<&'a Mapping, PlanError> {
    value
        .as_mapping()
        .ok_or_else(|| PlanError::Invalid(format!("invalid schema-5 {field}")))
}

pub(super) fn project(
    documents: &[PlannedDocument],
    target: u32,
) -> Result<BTreeMap<ProfileId, String>, PlanError> {
    let mut profiles = BTreeMap::<ProfileId, LegacyProfile>::new();
    for document in documents {
        let candidate = std::str::from_utf8(&document.candidate)
            .map_err(|_| PlanError::Invalid("converted profile is not UTF-8".to_owned()))?;
        let fifth = if target == 4 {
            schema_4_to_5::convert_profile_continuous(candidate)
                .map_err(|error| PlanError::Invalid(error.to_string()))?
        } else {
            candidate.to_owned()
        };
        let id = profile_facts(5, &fifth)?.0;
        let value: Value =
            serde_yaml::from_str(&fifth).map_err(|error| PlanError::Invalid(error.to_string()))?;
        let root = mapping(&value, "profile")?;
        let parameterless = root
            .get(key("parameters"))
            .is_none_or(|parameters| parameters.as_mapping().is_some_and(Mapping::is_empty));
        let includes = root
            .get(key("includes"))
            .and_then(Value::as_sequence)
            .into_iter()
            .flatten()
            .map(|item| {
                let item = mapping(item, "include")?;
                let child = item.get(key("id")).and_then(Value::as_str).ok_or_else(|| {
                    PlanError::Invalid(format!("profile {id} has invalid include ID"))
                })?;
                let alias = item.get(key("as")).and_then(Value::as_str).unwrap_or(child);
                Ok((child.to_owned(), alias.to_owned()))
            })
            .collect::<Result<Vec<_>, PlanError>>()?;
        let stages = root
            .get(key("stages"))
            .and_then(Value::as_mapping)
            .into_iter()
            .flat_map(Mapping::iter)
            .map(|(name, stage)| {
                let name = name
                    .as_str()
                    .ok_or_else(|| PlanError::Invalid("invalid stage ID".to_owned()))?;
                let after = mapping(stage, "stage")?
                    .get(key("after"))
                    .and_then(Value::as_sequence)
                    .ok_or_else(|| PlanError::Invalid("invalid stage after".to_owned()))?
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| PlanError::Invalid("invalid stage reference".to_owned()))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((name.to_owned(), after))
            })
            .collect::<Result<BTreeMap<_, _>, PlanError>>()?;
        let resource_mapping = mapping(
            root.get(key("resources"))
                .ok_or_else(|| PlanError::Invalid("missing resources".to_owned()))?,
            "resources",
        )?;
        let resources = resource_mapping
            .iter()
            .map(|(name, resource)| {
                let name = name
                    .as_str()
                    .ok_or_else(|| PlanError::Invalid("invalid resource ID".to_owned()))?;
                let depends = mapping(resource, "resource")?
                    .get(key("depends_on"))
                    .and_then(Value::as_sequence)
                    .into_iter()
                    .flatten()
                    .map(|item| {
                        item.as_str().map(str::to_owned).ok_or_else(|| {
                            PlanError::Invalid("invalid resource reference".to_owned())
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((name.to_owned(), depends))
            })
            .collect::<Result<BTreeMap<_, _>, PlanError>>()?;
        let resource_stages = resource_mapping
            .iter()
            .map(|(name, resource)| {
                let name = name
                    .as_str()
                    .ok_or_else(|| PlanError::Invalid("invalid resource ID".to_owned()))?;
                let stage = mapping(resource, "resource")?
                    .get(key("stage"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                Ok((name.to_owned(), stage))
            })
            .collect::<Result<BTreeMap<_, _>, PlanError>>()?;
        if profiles
            .insert(
                id,
                LegacyProfile {
                    value,
                    includes,
                    stages,
                    resources,
                    resource_stages,
                    parameterless,
                },
            )
            .is_some()
        {
            return Err(PlanError::Invalid("duplicate profile ID".to_owned()));
        }
    }

    for (root, profile) in &profiles {
        if !profile.parameterless {
            continue;
        }
        let mut instances = BTreeMap::<String, ProfileId>::new();
        let mut pending = vec![(root.to_string(), root.clone())];
        while let Some((path, id)) = pending.pop() {
            if let Some(previous) = instances.insert(path.clone(), id.clone())
                && previous != id
            {
                return Err(PlanError::Invalid(format!(
                    "ambiguous schema-5 instance {path}"
                )));
            }
            for (child, alias) in &profiles[&id].includes {
                let child_id = ProfileId::parse(child.clone())
                    .map_err(|error| PlanError::Invalid(error.to_string()))?;
                pending.push((format!("{path}/{alias}"), child_id));
            }
        }
        let mut edges = BTreeMap::<String, BTreeSet<String>>::new();
        let mut resource_edges = BTreeMap::<String, BTreeSet<String>>::new();
        let mut members = BTreeMap::<String, BTreeSet<String>>::new();
        for (path, id) in &instances {
            let profile = &profiles[id];
            for (stage, after) in &profile.stages {
                let owner = format!("{path}/{stage}");
                for reference in after {
                    let resolved = resolve(reference, path, &instances, &profiles, true)?;
                    edges.entry(owner.clone()).or_default().insert(resolved);
                }
            }
            for (resource, depends) in &profile.resources {
                let owner = format!("{path}/{resource}");
                if let Some(stage) = profile.resource_stages[resource].as_ref() {
                    members
                        .entry(format!("{path}/{stage}"))
                        .or_default()
                        .insert(owner.clone());
                }
                for reference in depends {
                    let predecessor = resolve(reference, path, &instances, &profiles, false)
                        .map_err(|error| {
                            PlanError::Invalid(format!("profile {id} resource {resource}: {error}"))
                        })?;
                    if predecessor == owner
                        || !resource_edges
                            .entry(owner.clone())
                            .or_default()
                            .insert(predecessor)
                    {
                        return Err(PlanError::Invalid(format!(
                            "profile {id} resource {resource} has a self or duplicate dependency {reference}"
                        )));
                    }
                }
            }
        }
        fn visit(
            node: &str,
            edges: &BTreeMap<String, BTreeSet<String>>,
            pending: &mut BTreeSet<String>,
            done: &mut BTreeSet<String>,
        ) -> Result<(), PlanError> {
            if done.contains(node) {
                return Ok(());
            }
            if !pending.insert(node.to_owned()) {
                return Err(PlanError::Invalid(format!(
                    "schema-5 stage cycle at {node}"
                )));
            }
            for next in edges.get(node).into_iter().flatten() {
                visit(next, edges, pending, done)?;
            }
            pending.remove(node);
            done.insert(node.to_owned());
            Ok(())
        }
        let mut pending = BTreeSet::new();
        let mut done = BTreeSet::new();
        for node in edges.keys() {
            visit(node, &edges, &mut pending, &mut done)?;
        }
        for (successor, predecessors) in &edges {
            let mut ancestors = BTreeSet::new();
            let mut search = predecessors.iter().cloned().collect::<Vec<_>>();
            while let Some(predecessor) = search.pop() {
                if ancestors.insert(predecessor.clone()) {
                    search.extend(edges.get(&predecessor).into_iter().flatten().cloned());
                }
            }
            for after_resource in members.get(successor).into_iter().flatten() {
                for predecessor in &ancestors {
                    for before_resource in members.get(predecessor).into_iter().flatten() {
                        resource_edges
                            .entry(after_resource.clone())
                            .or_default()
                            .insert(before_resource.clone());
                    }
                }
            }
        }
        pending.clear();
        done.clear();
        for node in resource_edges.keys() {
            visit(node, &resource_edges, &mut pending, &mut done)?;
        }
    }

    let mut projected = BTreeMap::new();
    for (id, mut profile) in profiles {
        let root = profile.value.as_mapping_mut().expect("validated mapping");
        root.insert(key("schema_version"), Value::Number(6.into()));
        root.remove(key("stages"));
        if let Some(resources) = root
            .get_mut(key("resources"))
            .and_then(Value::as_mapping_mut)
        {
            for resource in resources.values_mut() {
                if let Some(fields) = resource.as_mapping_mut() {
                    fields.remove(key("stage"));
                    fields.remove(key("depends_on"));
                }
            }
        }
        projected.insert(
            id,
            serde_yaml::to_string(&profile.value)
                .map_err(|error| PlanError::Invalid(error.to_string()))?,
        );
    }
    Ok(projected)
}

fn resolve(
    reference: &str,
    local: &str,
    instances: &BTreeMap<String, ProfileId>,
    profiles: &BTreeMap<ProfileId, LegacyProfile>,
    stage: bool,
) -> Result<String, PlanError> {
    let (path, name) = reference.rsplit_once('/').unwrap_or((local, reference));
    let id = instances.get(path).ok_or_else(|| {
        PlanError::Invalid(format!(
            "{reference} does not name a resolved stage instance"
        ))
    })?;
    let exists = if stage {
        profiles[id].stages.contains_key(name)
    } else {
        profiles[id].resources.contains_key(name)
    };
    if !exists {
        return Err(PlanError::Invalid(format!(
            "{reference} names no declared {}",
            if stage { "stage" } else { "resource" }
        )));
    }
    Ok(format!("{path}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn document(body: &str) -> PlannedDocument {
        let source = format!("schema_version: 5\nid: root\n{body}").into_bytes();
        PlannedDocument {
            path: PathBuf::from("root.yaml"),
            source_generation: 5,
            target_generation: 5,
            route: Vec::new(),
            source: source.clone(),
            candidate: source,
        }
    }

    fn resource(name: &str, stage: &str, depends: &str) -> String {
        format!(
            "  {name}:\n    type: file\n    stage: {stage}\n    depends_on: [{depends}]\n    properties: {{ kind: file, operation: link, source: {{ store: dotfiles, path: {name} }}, target: /tmp/{name} }}\n"
        )
    }

    #[test]
    fn legacy_projection_rejects_self_direct_and_combined_cycles() {
        let stages = "stages: { early: { after: [] }, late: { after: [early] } }\nresources:\n";
        for (body, expected) in [
            (
                format!("{stages}{}", resource("a", "early", "a")),
                "self or duplicate dependency",
            ),
            (
                format!(
                    "{stages}{}{}",
                    resource("a", "early", "b"),
                    resource("b", "early", "a")
                ),
                "cycle",
            ),
            (
                format!(
                    "{stages}{}{}",
                    resource("a", "early", "b"),
                    resource("b", "late", "")
                ),
                "cycle",
            ),
        ] {
            let error = project(&[document(&body)], 5).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }
}
