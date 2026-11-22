//! Private schema-5 profile conversion over a complete discovered inventory.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_yaml::{Mapping, Value};

use crate::declaration::profile::{FileResourceDeclaration, ParameterType, ValueOrReference};
use crate::domain::ids::{ProfileId, ResourceId};

use super::super::schema_4_to_5::replace_root_schema_version;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyProfile {
    schema_version: u32,
    id: String,
    #[serde(default)]
    parameters: BTreeMap<String, ParameterType>,
    #[serde(default)]
    includes: Vec<LegacyInclude>,
    #[serde(default)]
    stages: BTreeMap<String, LegacyStage>,
    resources: BTreeMap<String, FileResourceDeclaration>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyInclude {
    id: String,
    #[serde(default, rename = "as")]
    alias: Option<String>,
    #[serde(default, rename = "with")]
    bindings: BTreeMap<String, ValueOrReference>,
}

impl LegacyInclude {
    fn alias(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyStage {
    after: Vec<String>,
}

fn parse(source: &str) -> Result<LegacyProfile, String> {
    let profile: LegacyProfile = serde_yaml::from_str(source).map_err(|error| error.to_string())?;
    if profile.schema_version != 5 {
        return Err(format!("profile {} is not schema 5", profile.id));
    }
    ProfileId::parse(profile.id.clone()).map_err(|error| format!("profile ID: {error}"))?;
    for name in profile.parameters.keys() {
        ProfileId::parse(name.clone()).map_err(|error| format!("parameter {name}: {error}"))?;
    }
    for include in &profile.includes {
        ProfileId::parse(include.id.clone())
            .map_err(|error| format!("profile {} include {}: {error}", profile.id, include.id))?;
        ProfileId::parse(include.alias().to_owned()).map_err(|error| {
            format!(
                "profile {} include alias {}: {error}",
                profile.id,
                include.alias()
            )
        })?;
        for binding in include.bindings.keys() {
            ProfileId::parse(binding.clone())
                .map_err(|error| format!("profile {} binding {binding}: {error}", profile.id))?;
        }
    }
    for (name, stage) in &profile.stages {
        ResourceId::parse(name.clone())
            .map_err(|error| format!("profile {} stage {name}: {error}", profile.id))?;
        let mut seen = BTreeSet::new();
        for reference in &stage.after {
            validate_reference(reference)?;
            if !seen.insert(reference) {
                return Err(format!(
                    "profile {} stage {name} duplicates after {reference}",
                    profile.id
                ));
            }
        }
    }
    for (name, resource) in &profile.resources {
        ResourceId::parse(name.clone())
            .map_err(|error| format!("profile {} resource {name}: {error}", profile.id))?;
        if let Some(stage) = resource.stage() {
            ResourceId::parse(stage.to_owned()).map_err(|error| {
                format!(
                    "profile {} resource {name} stage {stage}: {error}",
                    profile.id
                )
            })?;
            if !profile.stages.contains_key(stage) {
                return Err(format!(
                    "profile {} resource {name} names undeclared local stage {stage}",
                    profile.id
                ));
            }
        }
        let mut seen = BTreeSet::new();
        for reference in resource.depends_on() {
            validate_reference(reference)?;
            if !seen.insert(reference) {
                return Err(format!(
                    "profile {} resource {name} duplicates depends_on {reference}",
                    profile.id
                ));
            }
        }
    }
    Ok(profile)
}

fn validate_reference(reference: &str) -> Result<(), String> {
    if reference
        .split('/')
        .all(|part| ResourceId::parse(part.to_owned()).is_ok())
    {
        Ok(())
    } else {
        Err(format!("invalid schema-5 ordering reference {reference:?}"))
    }
}

pub(crate) fn profile_facts(source: &str) -> Result<(ProfileId, Vec<ProfileId>), String> {
    let profile = parse(source)?;
    let id = ProfileId::parse(profile.id).map_err(|error| error.to_string())?;
    let includes = profile
        .includes
        .into_iter()
        .map(|include| ProfileId::parse(include.id).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((id, includes))
}

/// The single-document route covers documents without ordering declarations.
/// Ordering declarations require the complete inventory route below.
pub(crate) fn convert_profile(source: &str) -> Result<String, String> {
    let profile = parse(source)?;
    if !profile.stages.is_empty()
        || profile
            .resources
            .values()
            .any(|resource| resource.stage().is_some() || !resource.depends_on().is_empty())
    {
        return Err(format!(
            "profile {} has schema-5 ordering; convert the complete inventory",
            profile.id
        ));
    }
    replace_root_schema_version(source, '5', '6').map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProfileConversion {
    pub(crate) documents: BTreeMap<ProfileId, String>,
    pub(crate) broadening: Vec<String>,
}

#[derive(Clone, Debug)]
struct Instance {
    profile: ProfileId,
    path: String,
}

fn contexts(
    root: &ProfileId,
    old: &BTreeMap<ProfileId, LegacyProfile>,
) -> Result<BTreeMap<String, Instance>, String> {
    fn visit(
        id: &ProfileId,
        path: String,
        old: &BTreeMap<ProfileId, LegacyProfile>,
        result: &mut BTreeMap<String, Instance>,
        stack: &mut BTreeSet<ProfileId>,
    ) -> Result<(), String> {
        if !stack.insert(id.clone()) {
            return Err(format!("include cycle at profile {id}"));
        }
        if let Some(existing) = result.insert(
            path.clone(),
            Instance {
                profile: id.clone(),
                path: path.clone(),
            },
        ) && existing.profile != *id
        {
            return Err(format!("instance {path} has conflicting definitions"));
        }
        for include in &old[id].includes {
            let child = ProfileId::parse(include.id.clone()).map_err(|e| e.to_string())?;
            if !old.contains_key(&child) {
                return Err(format!("profile {id} includes missing profile {child}"));
            }
            visit(
                &child,
                format!("{path}/{}", include.alias()),
                old,
                result,
                stack,
            )?;
        }
        stack.remove(id);
        Ok(())
    }
    let mut result = BTreeMap::new();
    visit(
        root,
        root.to_string(),
        old,
        &mut result,
        &mut BTreeSet::new(),
    )?;
    Ok(result)
}

fn endpoint(
    reference: &str,
    local_path: &str,
    instances: &BTreeMap<String, Instance>,
    old: &BTreeMap<ProfileId, LegacyProfile>,
    stage: bool,
) -> Result<(String, String), String> {
    let (path, name) = match reference.rsplit_once('/') {
        Some((path, name)) => (path.to_owned(), name.to_owned()),
        None => (local_path.to_owned(), reference.to_owned()),
    };
    let instance = instances
        .get(&path)
        .ok_or_else(|| format!("reference {reference} names no instance in selected root"))?;
    let exists = if stage {
        old[&instance.profile].stages.contains_key(&name)
    } else {
        old[&instance.profile].resources.contains_key(&name)
    };
    if !exists {
        return Err(format!(
            "reference {reference} names no declared {}",
            if stage { "stage" } else { "resource" }
        ));
    }
    Ok((path, name))
}

fn yaml_key(key: &str) -> Value {
    Value::String(key.to_owned())
}

fn root_mapping(value: &mut Value) -> Result<&mut Mapping, String> {
    value
        .as_mapping_mut()
        .ok_or_else(|| "profile must be a mapping".to_owned())
}

fn set_stages(
    value: &mut Value,
    stages: &BTreeMap<String, BTreeSet<String>>,
) -> Result<(), String> {
    let root = root_mapping(value)?;
    if stages.is_empty() {
        root.remove(yaml_key("stages"));
        return Ok(());
    }
    let mut mapping = Mapping::new();
    for (name, after) in stages {
        let mut stage = Mapping::new();
        stage.insert(
            yaml_key("after"),
            Value::Sequence(after.iter().cloned().map(Value::String).collect()),
        );
        mapping.insert(yaml_key(name), Value::Mapping(stage));
    }
    root.insert(yaml_key("stages"), Value::Mapping(mapping));
    Ok(())
}

/// Converts the schema-5 side of a complete in-memory inventory.
/// Inputs with generation 6 are retained byte-for-byte and participate in root contexts.
pub(crate) fn convert_inventory(
    profiles: &BTreeMap<ProfileId, (u32, String)>,
) -> Result<ProfileConversion, String> {
    let mut old = BTreeMap::new();
    let mut values = BTreeMap::new();
    let mut result = ProfileConversion::default();
    let mut existing_edges = BTreeMap::<ProfileId, BTreeSet<(String, String)>>::new();
    for (id, (generation, source)) in profiles {
        if *generation == 5 {
            let parsed = parse(source).map_err(|error| format!("profile {id}: {error}"))?;
            if parsed.id != id.as_str() {
                return Err(format!(
                    "profile {id} has mismatched declared ID {}",
                    parsed.id
                ));
            }
            old.insert(id.clone(), parsed);
            let value: Value = serde_yaml::from_str(source).map_err(|error| error.to_string())?;
            values.insert(id.clone(), value);
        } else if *generation == 6 {
            result.documents.insert(id.clone(), source.clone());
        } else {
            return Err(format!("profile {id} must first advance to schema 5"));
        }
    }
    // A schema-6 document is represented for context traversal only; it is never rewritten.
    for (id, (generation, source)) in profiles {
        if *generation == 6 {
            let parsed: Value = serde_yaml::from_str(source).map_err(|error| error.to_string())?;
            let mapping = parsed.as_mapping().ok_or("profile must be a mapping")?;
            let includes: Vec<LegacyInclude> = mapping
                .get(yaml_key("includes"))
                .map(|value| {
                    let mut stripped = value.clone();
                    for include in stripped
                        .as_sequence_mut()
                        .ok_or("includes must be a sequence")?
                    {
                        include
                            .as_mapping_mut()
                            .ok_or("include must be a mapping")?
                            .remove(yaml_key("stage"));
                    }
                    serde_yaml::from_value(stripped).map_err(|e| e.to_string())
                })
                .transpose()?
                .unwrap_or_default();
            let parameters: BTreeMap<String, ParameterType> = mapping
                .get(yaml_key("parameters"))
                .map(|value| serde_yaml::from_value(value.clone()).map_err(|e| e.to_string()))
                .transpose()?
                .unwrap_or_default();
            let stages: BTreeMap<String, LegacyStage> = mapping
                .get(yaml_key("stages"))
                .map(|value| serde_yaml::from_value(value.clone()).map_err(|e| e.to_string()))
                .transpose()?
                .unwrap_or_default();
            let resources: BTreeMap<String, FileResourceDeclaration> = mapping
                .get(yaml_key("resources"))
                .map(|value| serde_yaml::from_value(value.clone()).map_err(|e| e.to_string()))
                .transpose()?
                .unwrap_or_default();
            let edges: Vec<BTreeMap<String, String>> = mapping
                .get(yaml_key("resource_dependencies"))
                .map(|value| serde_yaml::from_value(value.clone()).map_err(|e| e.to_string()))
                .transpose()?
                .unwrap_or_default();
            let mut exact_edges = BTreeSet::new();
            for edge in edges {
                if edge.len() != 2 {
                    return Err(format!(
                        "schema-6 profile {id} has invalid resource dependency"
                    ));
                }
                let before = edge
                    .get("before")
                    .ok_or_else(|| format!("schema-6 profile {id} misses dependency before"))?;
                let after = edge
                    .get("after")
                    .ok_or_else(|| format!("schema-6 profile {id} misses dependency after"))?;
                exact_edges.insert((before.clone(), after.clone()));
            }
            existing_edges.insert(id.clone(), exact_edges);
            old.insert(
                id.clone(),
                LegacyProfile {
                    schema_version: 6,
                    id: id.to_string(),
                    parameters,
                    includes,
                    stages,
                    resources,
                },
            );
        }
    }
    let mut additions = BTreeMap::<ProfileId, BTreeMap<String, BTreeSet<String>>>::new();
    let mut dependencies = BTreeMap::<ProfileId, BTreeSet<(String, String)>>::new();
    let mut dependency_placements =
        BTreeMap::<(ProfileId, String, String), (ProfileId, String, String)>::new();
    let mut local_rewrites = BTreeMap::<(ProfileId, String, String), String>::new();
    let roots = old
        .iter()
        .filter(|(_, profile)| profile.parameters.is_empty())
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let mut dual_use_contexts = BTreeMap::<ProfileId, BTreeSet<String>>::new();
    for root in &roots {
        for instance in contexts(root, &old)?.values() {
            if instance.profile == *root {
                continue;
            }
            let profile = &old[&instance.profile];
            if profile.schema_version == 5
                && profile.parameters.is_empty()
                && !profile.stages.is_empty()
            {
                dual_use_contexts
                    .entry(instance.profile.clone())
                    .or_default()
                    .insert(format!("root {root} include path {}", instance.path));
            }
        }
    }
    if let Some((child, contexts)) = dual_use_contexts.into_iter().next() {
        let stages = old[&child]
            .stages
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "schema-5 profile {child} declares stages [{stages}] as a standalone root and is also included in {}; schema 6 permits stages only on the selected root. Create a dedicated root profile for standalone use and move these phase declarations to the owning roots before migration",
            contexts.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    for root in roots {
        let instances = contexts(&root, &old)?;
        let mut phases = BTreeMap::<String, BTreeSet<String>>::new();
        let mut phase_origin = BTreeMap::<String, String>::new();
        for instance in instances.values() {
            let profile = &old[&instance.profile];
            for (name, stage) in &profile.stages {
                let mut after = BTreeSet::new();
                for reference in &stage.after {
                    let (_, predecessor) =
                        endpoint(reference, &instance.path, &instances, &old, true).map_err(
                            |error| {
                                format!(
                                    "profile {} stage {name} in root {root}: {error}",
                                    instance.profile
                                )
                            },
                        )?;
                    if predecessor == *name || !after.insert(predecessor.clone()) {
                        return Err(format!(
                            "profile {} stage {name} in root {root} has duplicate or self after {reference}",
                            instance.profile
                        ));
                    }
                }
                if let Some(previous) = phases.insert(name.clone(), after.clone())
                    && previous != after
                {
                    return Err(format!(
                        "root {root} merges stage {name} with different after relations from {} and {}",
                        phase_origin[name], instance.profile
                    ));
                }
                phase_origin.insert(name.clone(), instance.profile.to_string());
            }
        }
        if old[&root].schema_version == 6 {
            for (name, after) in &phases {
                let Some(stage) = old[&root].stages.get(name) else {
                    return Err(format!(
                        "schema-6 root {root} lacks phase {name} required by schema-5 child; edit the inventory manually"
                    ));
                };
                if stage.after.iter().cloned().collect::<BTreeSet<_>>() != *after {
                    return Err(format!(
                        "schema-6 root {root} phase {name} differs from schema-5 child after relation; edit the inventory manually"
                    ));
                }
            }
        }
        if old[&root].schema_version == 5 {
            additions.insert(root.clone(), phases.clone());
        }
        let mut old_stage_edges = BTreeMap::<String, BTreeSet<String>>::new();
        let mut phase_members = BTreeMap::<String, Vec<(String, String)>>::new();
        for instance in instances.values() {
            let profile = &old[&instance.profile];
            for (name, stage) in &profile.stages {
                let successor = format!("{}/{}", instance.path, name);
                for reference in &stage.after {
                    let (path, predecessor) =
                        endpoint(reference, &instance.path, &instances, &old, true)?;
                    old_stage_edges
                        .entry(format!("{path}/{predecessor}"))
                        .or_default()
                        .insert(successor.clone());
                }
            }
            for (name, resource) in &profile.resources {
                if let Some(phase) = resource.stage() {
                    phase_members.entry(phase.to_owned()).or_default().push((
                        format!("{}/{}", instance.path, name),
                        format!("{}/{}", instance.path, phase),
                    ));
                }
            }
        }
        let mut old_resource_edges = BTreeMap::<String, BTreeSet<String>>::new();
        let mut old_stage_members = BTreeMap::<String, Vec<String>>::new();
        for instance in instances.values() {
            for (name, resource) in &old[&instance.profile].resources {
                let successor = format!("{}/{}", instance.path, name);
                if let Some(stage) = resource.stage() {
                    old_stage_members
                        .entry(format!("{}/{}", instance.path, stage))
                        .or_default()
                        .push(successor.clone());
                }
                for reference in resource.depends_on() {
                    let (path, predecessor) =
                        endpoint(reference, &instance.path, &instances, &old, false).map_err(
                            |error| {
                                format!(
                                    "profile {} resource {name} in root {root}: {error}",
                                    instance.profile
                                )
                            },
                        )?;
                    old_resource_edges
                        .entry(format!("{path}/{predecessor}"))
                        .or_default()
                        .insert(successor.clone());
                }
            }
        }
        for predecessor in old_stage_members.keys() {
            let mut pending = old_stage_edges
                .get(predecessor)
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            let mut successors = BTreeSet::new();
            while let Some(successor) = pending.pop() {
                if !successors.insert(successor.clone()) {
                    continue;
                }
                pending.extend(
                    old_stage_edges
                        .get(&successor)
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
            for successor in successors {
                for before in old_stage_members.get(predecessor).into_iter().flatten() {
                    for after in old_stage_members.get(&successor).into_iter().flatten() {
                        old_resource_edges
                            .entry(before.clone())
                            .or_default()
                            .insert(after.clone());
                    }
                }
            }
        }
        for successor in phases.keys() {
            if old[&root].schema_version == 6 {
                continue;
            }
            let mut pending = phases[successor]
                .iter()
                .map(|predecessor| {
                    (
                        predecessor.clone(),
                        vec![predecessor.clone(), successor.clone()],
                    )
                })
                .collect::<Vec<_>>();
            let mut predecessors = BTreeMap::<String, Vec<String>>::new();
            while let Some((predecessor, path)) = pending.pop() {
                if predecessors.contains_key(&predecessor) {
                    continue;
                }
                for earlier in phases.get(&predecessor).into_iter().flatten() {
                    let mut earlier_path = Vec::with_capacity(path.len() + 1);
                    earlier_path.push(earlier.clone());
                    earlier_path.extend(path.iter().cloned());
                    pending.push((earlier.clone(), earlier_path));
                }
                predecessors.insert(predecessor, path);
            }
            for (predecessor, path) in predecessors {
                let mut newly_covered = Vec::new();
                for (before_resource, _) in phase_members.get(&predecessor).into_iter().flatten() {
                    for (after_resource, _) in phase_members.get(successor).into_iter().flatten() {
                        if !reachable(before_resource, after_resource, &old_resource_edges) {
                            newly_covered.push(format!("{before_resource} -> {after_resource}"));
                        }
                    }
                }
                if !newly_covered.is_empty() {
                    result.broadening.push(format!(
                        "root {root}: phase {} newly orders {}",
                        path.join(" -> "),
                        newly_covered.join(", ")
                    ));
                }
            }
        }
        for instance in instances.values() {
            let profile = &old[&instance.profile];
            if profile.schema_version != 5 {
                continue;
            }
            for (name, resource) in &profile.resources {
                for reference in resource.depends_on() {
                    let (pre_path, pre_name) =
                        endpoint(reference, &instance.path, &instances, &old, false).map_err(
                            |error| {
                                format!(
                                    "profile {} resource {name} in root {root}: {error}",
                                    instance.profile
                                )
                            },
                        )?;
                    if pre_path == instance.path {
                        if reference.contains('/') {
                            let key = (instance.profile.clone(), name.clone(), reference.clone());
                            if let Some(previous) = local_rewrites.insert(key, pre_name.clone())
                                && previous != pre_name
                            {
                                return Err(format!(
                                    "profile {} resource {name} depends_on {reference} has different local meanings in root {root}",
                                    instance.profile
                                ));
                            }
                        }
                        continue;
                    }
                    let parent = common_parent(&pre_path, &instance.path)?;
                    let owner = &instances[&parent].profile;
                    let before = relative(&parent, &pre_path, &pre_name);
                    let after = relative(&parent, &instance.path, name);
                    if old[owner].schema_version == 6
                        && !existing_edges
                            .get(owner)
                            .is_some_and(|edges| edges.contains(&(before.clone(), after.clone())))
                    {
                        return Err(format!(
                            "profile {} resource {name} in root {root} needs missing dependency {before} -> {after} in schema-6 parent {owner}; edit the inventory manually",
                            instance.profile
                        ));
                    }
                    let declaration = (instance.profile.clone(), name.clone(), reference.clone());
                    let placement = (owner.clone(), before.clone(), after.clone());
                    if let Some(previous) =
                        dependency_placements.insert(declaration.clone(), placement.clone())
                        && previous != placement
                    {
                        return Err(format!(
                            "profile {} resource {name} depends_on {reference} has different placements in root {root}: {:?} and {:?}; edit the declaration manually",
                            instance.profile, previous, placement
                        ));
                    }
                    dependencies
                        .entry(owner.clone())
                        .or_default()
                        .insert((before, after));
                }
            }
        }
    }
    for (id, value) in &mut values {
        let stages = additions.remove(id).unwrap_or_default();
        if stages.is_empty()
            && !dependencies.contains_key(id)
            && let Some((_, source)) = profiles.get(id)
            && let Ok(converted) = convert_profile(source)
        {
            result.documents.insert(id.clone(), converted);
            continue;
        }
        set_stages(value, &stages)?;
        let root = root_mapping(value)?;
        root.insert(yaml_key("schema_version"), Value::Number(6.into()));
        if let Some(resources) = root
            .get_mut(yaml_key("resources"))
            .and_then(Value::as_mapping_mut)
        {
            for (name, resource) in resources {
                let Some(fields) = resource.as_mapping_mut() else {
                    continue;
                };
                if let Some(Value::Sequence(refs)) = fields.get_mut(yaml_key("depends_on")) {
                    let mut rewritten = Vec::new();
                    for reference in refs.iter() {
                        let Some(text) = reference.as_str() else {
                            continue;
                        };
                        if !text.contains('/') {
                            rewritten.push(Value::String(text.to_owned()));
                        } else if let Some(local) = local_rewrites.get(&(
                            id.clone(),
                            name.as_str().unwrap_or_default().to_owned(),
                            text.to_owned(),
                        )) {
                            rewritten.push(Value::String(local.clone()));
                        }
                    }
                    *refs = rewritten;
                }
            }
        }
        if let Some(edges) = dependencies.get(id) {
            let mut sequence = Vec::new();
            for (before, after) in edges {
                let mut edge = Mapping::new();
                edge.insert(yaml_key("before"), Value::String(before.clone()));
                edge.insert(yaml_key("after"), Value::String(after.clone()));
                sequence.push(Value::Mapping(edge));
            }
            root.insert(yaml_key("resource_dependencies"), Value::Sequence(sequence));
        }
        result.documents.insert(
            id.clone(),
            serde_yaml::to_string(value).map_err(|error| error.to_string())?,
        );
    }
    Ok(result)
}

fn common_parent(left: &str, right: &str) -> Result<String, String> {
    let parts = left
        .split('/')
        .zip(right.split('/'))
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return Err(format!(
            "resources {left} and {right} have no common parent"
        ));
    }
    if parts.len() > 1 {
        return Err(format!(
            "resources {left} and {right} have multiple parent instances containing both endpoints ({} and {}); move or edit the dependency manually",
            parts[0],
            parts.join("/")
        ));
    }
    Ok(parts.join("/"))
}

fn relative(parent: &str, path: &str, name: &str) -> String {
    if parent == path {
        name.to_owned()
    } else {
        format!(
            "{}/{}",
            path.strip_prefix(parent).unwrap().trim_start_matches('/'),
            name
        )
    }
}

fn reachable(before: &str, after: &str, edges: &BTreeMap<String, BTreeSet<String>>) -> bool {
    let mut pending = vec![before.to_owned()];
    let mut visited = BTreeSet::new();
    while let Some(current) = pending.pop() {
        if current == after {
            return true;
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        pending.extend(edges.get(&current).into_iter().flatten().cloned());
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> ProfileId {
        ProfileId::parse(value.to_owned()).unwrap()
    }

    fn inventory(entries: &[(&str, &str)]) -> BTreeMap<ProfileId, (u32, String)> {
        entries
            .iter()
            .map(|(name, text)| (id(name), (5, (*text).to_owned())))
            .collect()
    }

    const PROPERTIES: &str = "type: file\n    properties: {kind: file, operation: link, source: {store: dotfiles, path: item}, target: /tmp/item}";

    #[test]
    fn version_only_conversion_preserves_authoring_bytes() {
        let source = "# header\nschema_version: 5 # generation\nid: base\nresources: {}\n";
        let converted = convert_inventory(&inventory(&[("base", source)])).unwrap();
        assert_eq!(
            converted.documents[&id("base")],
            source.replacen('5', "6", 1)
        );
        assert!(converted.broadening.is_empty());
    }

    #[test]
    fn child_stages_move_to_root_and_report_actual_broadening() {
        let first = format!(
            "schema_version: 5\nid: first\nparameters: {{x: string}}\nstages: {{early: {{after: []}}, late: {{after: [early]}}}}\nresources:\n  a:\n    stage: early\n    {PROPERTIES}\n  b:\n    stage: late\n    {PROPERTIES}\n"
        );
        let second = format!(
            "schema_version: 5\nid: second\nparameters: {{x: string}}\nstages: {{early: {{after: []}}, late: {{after: [early]}}}}\nresources:\n  c:\n    stage: early\n    {PROPERTIES}\n  d:\n    stage: late\n    {PROPERTIES}\n"
        );
        let root = "schema_version: 5\nid: root\nincludes: [{id: first, with: {x: a}}, {id: second, with: {x: b}}]\nresources: {}\n";
        let converted = convert_inventory(&inventory(&[
            ("root", root),
            ("first", &first),
            ("second", &second),
        ]))
        .unwrap();
        let root_value: Value = serde_yaml::from_str(&converted.documents[&id("root")]).unwrap();
        assert!(
            root_value["stages"]["late"]["after"]
                .as_sequence()
                .unwrap()
                .contains(&yaml_key("early"))
        );
        assert!(
            converted
                .broadening
                .iter()
                .any(|notice| notice.contains("root root")
                    && notice.contains("root/first/a -> root/second/d"))
        );
    }

    #[test]
    fn standalone_and_included_stage_owner_requires_manual_root_split() {
        let child = format!(
            "schema_version: 5\nid: child\nstages: {{early: {{after: []}}}}\nresources:\n  item:\n    stage: early\n    {PROPERTIES}\n"
        );
        let parent = "schema_version: 5\nid: parent\nincludes: [{id: child}]\nresources: {}\n";
        let error =
            convert_inventory(&inventory(&[("child", &child), ("parent", parent)])).unwrap_err();
        for expected in [
            "profile child",
            "stages [early]",
            "standalone root",
            "root parent include path parent/child",
            "Create a dedicated root profile",
        ] {
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn conflicting_merged_after_rejects() {
        let first = "schema_version: 5\nid: first\nparameters: {x: string}\nstages: {early: {after: []}, late: {after: [early]}}\nresources: {}\n";
        let second = "schema_version: 5\nid: second\nparameters: {x: string}\nstages: {early: {after: []}, late: {after: []}}\nresources: {}\n";
        let root = "schema_version: 5\nid: root\nincludes: [{id: first, with: {x: a}}, {id: second, with: {x: b}}]\nresources: {}\n";
        let error = convert_inventory(&inventory(&[
            ("root", root),
            ("first", first),
            ("second", second),
        ]))
        .unwrap_err();
        assert!(error.contains("root root merges stage late"), "{error}");
    }

    #[test]
    fn cross_instance_dependency_moves_to_unique_parent() {
        let first = format!(
            "schema_version: 5\nid: first\nparameters: {{x: string}}\nresources:\n  a:\n    {PROPERTIES}\n"
        );
        let second = format!(
            "schema_version: 5\nid: second\nparameters: {{x: string}}\nresources:\n  b:\n    depends_on: [root/first/a]\n    {PROPERTIES}\n"
        );
        let root = "schema_version: 5\nid: root\nincludes: [{id: first, with: {x: a}}, {id: second, with: {x: b}}]\nresources: {}\n";
        let converted = convert_inventory(&inventory(&[
            ("root", root),
            ("first", &first),
            ("second", &second),
        ]))
        .unwrap();
        let root_value: Value = serde_yaml::from_str(&converted.documents[&id("root")]).unwrap();
        let edge = &root_value["resource_dependencies"][0];
        assert_eq!(edge["before"], yaml_key("first/a"));
        assert_eq!(edge["after"], yaml_key("second/b"));
        let child: Value = serde_yaml::from_str(&converted.documents[&id("second")]).unwrap();
        assert!(
            child["resources"]["b"]["depends_on"]
                .as_sequence()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn qualified_after_resolves_to_root_phase_without_false_broadening() {
        let first = format!(
            "schema_version: 5\nid: first\nparameters: {{x: string}}\nstages: {{early: {{after: []}}}}\nresources:\n  a:\n    stage: early\n    {PROPERTIES}\n"
        );
        let second = format!(
            "schema_version: 5\nid: second\nparameters: {{x: string}}\nstages: {{late: {{after: [root/first/early]}}}}\nresources:\n  b:\n    stage: late\n    {PROPERTIES}\n"
        );
        let root = "schema_version: 5\nid: root\nincludes: [{id: first, with: {x: a}}, {id: second, with: {x: b}}]\nresources: {}\n";
        let converted = convert_inventory(&inventory(&[
            ("root", root),
            ("first", &first),
            ("second", &second),
        ]))
        .unwrap();
        let root_value: Value = serde_yaml::from_str(&converted.documents[&id("root")]).unwrap();
        assert_eq!(root_value["stages"]["late"]["after"][0], yaml_key("early"));
        assert!(
            converted.broadening.is_empty(),
            "{:?}",
            converted.broadening
        );
    }

    #[test]
    fn differing_root_contexts_reject_dependency_relocation() {
        let first = format!(
            "schema_version: 5\nid: first\nparameters: {{x: string}}\nresources:\n  a:\n    {PROPERTIES}\n"
        );
        let second = format!(
            "schema_version: 5\nid: second\nparameters: {{x: string}}\nresources:\n  b:\n    depends_on: [root-one/first/a]\n    {PROPERTIES}\n"
        );
        let one = "schema_version: 5\nid: root-one\nincludes: [{id: first, with: {x: a}}, {id: second, with: {x: b}}]\nresources: {}\n";
        let two = "schema_version: 5\nid: root-two\nincludes: [{id: first, with: {x: a}}, {id: second, with: {x: b}}]\nresources: {}\n";
        let error = convert_inventory(&inventory(&[
            ("root-one", one),
            ("root-two", two),
            ("first", &first),
            ("second", &second),
        ]))
        .unwrap_err();
        assert!(
            error.contains("root-two") && error.contains("root-one/first/a"),
            "{error}"
        );
    }

    #[test]
    fn nested_common_ancestors_reject_ambiguous_dependency_parent() {
        let first = format!(
            "schema_version: 5\nid: first\nparameters: {{x: string}}\nresources:\n  a:\n    {PROPERTIES}\n"
        );
        let second = format!(
            "schema_version: 5\nid: second\nparameters: {{x: string}}\nresources:\n  b:\n    depends_on: [root/parent/first/a]\n    {PROPERTIES}\n"
        );
        let parent = "schema_version: 5\nid: parent\nparameters: {x: string}\nincludes: [{id: first, with: {x: {parameter: x}}}, {id: second, with: {x: {parameter: x}}}]\nresources: {}\n";
        let root =
            "schema_version: 5\nid: root\nincludes: [{id: parent, with: {x: a}}]\nresources: {}\n";
        let error = convert_inventory(&inventory(&[
            ("root", root),
            ("parent", parent),
            ("first", &first),
            ("second", &second),
        ]))
        .unwrap_err();
        assert!(
            error.contains("multiple parent instances") && error.contains("root/parent"),
            "{error}"
        );
    }

    #[test]
    fn old_direct_edge_prevents_false_broadening_notice() {
        let first = format!(
            "schema_version: 5\nid: first\nparameters: {{x: string}}\nstages: {{early: {{after: []}}}}\nresources:\n  a:\n    stage: early\n    {PROPERTIES}\n"
        );
        let second = format!(
            "schema_version: 5\nid: second\nparameters: {{x: string}}\nstages: {{early: {{after: []}}, late: {{after: [early]}}}}\nresources:\n  b:\n    stage: late\n    depends_on: [root/first/a]\n    {PROPERTIES}\n"
        );
        let root = "schema_version: 5\nid: root\nincludes: [{id: first, with: {x: a}}, {id: second, with: {x: b}}]\nstages: {early: {after: []}, late: {after: [early]}}\nresources: {}\n";
        let converted = convert_inventory(&inventory(&[
            ("root", root),
            ("first", &first),
            ("second", &second),
        ]))
        .unwrap();
        assert!(
            converted.broadening.is_empty(),
            "{:?}",
            converted.broadening
        );
    }

    #[test]
    fn empty_intermediate_phase_reports_cross_child_broadening() {
        let first = format!(
            "schema_version: 5\nid: first\nparameters: {{x: string}}\nstages: {{a: {{after: []}}, b: {{after: [a]}}, c: {{after: [b]}}}}\nresources:\n  early:\n    stage: a\n    {PROPERTIES}\n"
        );
        let second = format!(
            "schema_version: 5\nid: second\nparameters: {{x: string}}\nstages: {{a: {{after: []}}, b: {{after: [a]}}, c: {{after: [b]}}}}\nresources:\n  late:\n    stage: c\n    {PROPERTIES}\n"
        );
        let root = "schema_version: 5\nid: root\nincludes: [{id: first, with: {x: a}}, {id: second, with: {x: b}}]\nresources: {}\n";
        let converted = convert_inventory(&inventory(&[
            ("root", root),
            ("first", &first),
            ("second", &second),
        ]))
        .unwrap();
        assert!(
            converted.broadening.iter().any(|notice| {
                notice.contains("root root: phase a -> b -> c")
                    && notice.contains("root/first/early -> root/second/late")
            }),
            "{:?}",
            converted.broadening
        );
    }

    #[test]
    fn empty_intermediate_phase_does_not_report_existing_local_order() {
        let child = format!(
            "schema_version: 5\nid: child\nparameters: {{x: string}}\nstages: {{a: {{after: []}}, b: {{after: [a]}}, c: {{after: [b]}}}}\nresources:\n  early:\n    stage: a\n    {PROPERTIES}\n  late:\n    stage: c\n    {PROPERTIES}\n"
        );
        let root =
            "schema_version: 5\nid: root\nincludes: [{id: child, with: {x: a}}]\nresources: {}\n";
        let converted =
            convert_inventory(&inventory(&[("root", root), ("child", &child)])).unwrap();
        assert!(
            converted.broadening.is_empty(),
            "{:?}",
            converted.broadening
        );
    }

    #[test]
    fn resumes_after_root_was_published_before_child() {
        let child = format!(
            "schema_version: 5\nid: child\nparameters: {{x: string}}\nstages: {{early: {{after: []}}, late: {{after: [early]}}}}\nresources:\n  a:\n    stage: early\n    {PROPERTIES}\n  b:\n    stage: late\n    {PROPERTIES}\n"
        );
        let root = "schema_version: 6\nid: root\nincludes: [{id: child, with: {x: a}}]\nstages: {early: {after: []}, late: {after: [early]}}\nresources: {}\n";
        let mut input = inventory(&[("child", &child)]);
        input.insert(id("root"), (6, root.to_owned()));
        let converted = convert_inventory(&input).unwrap();
        assert_eq!(converted.documents[&id("root")], root);
        let child_value: Value = serde_yaml::from_str(&converted.documents[&id("child")]).unwrap();
        assert_eq!(child_value["schema_version"], Value::Number(6.into()));
        assert!(child_value.get("stages").is_none());
        assert!(converted.broadening.is_empty());
    }
}
