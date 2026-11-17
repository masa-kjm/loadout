//! Planning and confirmed publication for registered adjacent managed-document migrations.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::declaration::environment_config::EnvironmentConfig;
use crate::domain::ids::ProfileId;
use crate::domain::paths::ResolvedPath;
use crate::filesystem::is_link_or_reparse_point;
use crate::resolver::ResolverContext;
use crate::schema_generation::CURRENT_MANAGED_GENERATION;

use super::{schema_3_to_4, schema_4_to_5};

mod profiles;
mod state;

type ProfileRoute = fn(&str) -> Result<String, String>;
type StateRoute = fn(&[u8]) -> Result<Vec<u8>, String>;

#[derive(Clone, Copy)]
struct Route {
    from: u32,
    to: u32,
    profile: ProfileRoute,
    state: StateRoute,
}

fn routes() -> [Route; 2] {
    [
        Route {
            from: 3,
            to: 4,
            profile: |source| schema_3_to_4::convert_profile(source).map_err(|e| e.to_string()),
            state: |source| schema_3_to_4::convert_state(source).map_err(|e| e.to_string()),
        },
        Route {
            from: 4,
            to: 5,
            profile: |source| {
                schema_4_to_5::convert_profile_continuous(source).map_err(|e| e.to_string())
            },
            state: |source| schema_4_to_5::convert_state(source).map_err(|e| e.to_string()),
        },
    ]
}

fn select_routes(registry: &[Route], from: u32, target: u32) -> Result<Vec<Route>, PlanError> {
    let mut seen = BTreeSet::new();
    for route in registry {
        if route.to != route.from + 1 {
            return Err(PlanError::InvalidRoute);
        }
        if !seen.insert(route.from) {
            return Err(PlanError::DuplicateRoute(route.from));
        }
    }
    let latest = registry.iter().map(|route| route.to).max().unwrap_or(3);
    if target < 4 || target > latest || from < 3 || from > target {
        return Err(PlanError::UnsupportedGeneration { from, target });
    }
    let mut selected = Vec::new();
    for generation in from..target {
        let route = *registry
            .iter()
            .find(|route| route.from == generation)
            .ok_or(PlanError::MissingRoute(generation))?;
        selected.push(route);
    }
    Ok(selected)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PlannedDocument {
    pub(super) path: PathBuf,
    pub(super) source_generation: u32,
    pub(super) target_generation: u32,
    pub(super) route: Vec<(u32, u32)>,
    pub(super) source: Vec<u8>,
    pub(super) candidate: Vec<u8>,
}

impl PlannedDocument {
    pub(super) fn needs_publication(&self) -> bool {
        !self.route.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscoverySnapshot {
    declared_path: PathBuf,
    physical_path: PathBuf,
    entries: BTreeMap<PathBuf, EntryKind>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Regular,
    Directory,
    Link,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MigrationPlan {
    pub(super) target_generation: u32,
    configuration_path: PathBuf,
    configuration_bytes: Vec<u8>,
    state_path: PathBuf,
    discovery: Vec<DiscoverySnapshot>,
    pub(super) state: Option<PlannedDocument>,
    pub(super) profiles: Vec<PlannedDocument>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PresentedDocument {
    pub(super) path: PathBuf,
    pub(super) source_generation: Option<u32>,
    pub(super) target_generation: u32,
    pub(super) route: Vec<(u32, u32)>,
    pub(super) will_publish: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PlanPresentation {
    pub(super) configuration_path: PathBuf,
    pub(super) documents: Vec<PresentedDocument>,
    pub(super) normal_commands_available: bool,
}

impl MigrationPlan {
    pub(super) fn confirmation_fingerprint(&self) -> [u8; 32] {
        Sha256::digest(format!("{self:?}").as_bytes()).into()
    }

    pub(super) fn execute_confirmed(&mut self, context: &ResolverContext) -> Result<(), PlanError> {
        self.execute_confirmed_with_hook(context, |_| {})
    }

    fn execute_confirmed_with_hook(
        &mut self,
        context: &ResolverContext,
        mut after_publication: impl FnMut(&Path),
    ) -> Result<(), PlanError> {
        self.compare_confirmed(context)?;
        if let Some(document) = self.state.clone().filter(|item| item.needs_publication()) {
            state::publish(self, context, &document)?;
            self.record_verified_publication(&document.path)?;
            after_publication(&document.path);
        }
        if let Some(state) = &self.state {
            if state.source_generation != self.target_generation
                || read_regular(&state.path)? != state.candidate
            {
                return Err(PlanError::ConfirmedSnapshotChanged);
            }
        }
        for document in self
            .profiles
            .clone()
            .into_iter()
            .filter(|item| item.needs_publication())
        {
            self.compare_confirmed(context)?;
            profiles::publish(&document)?;
            self.record_verified_publication(&document.path)?;
            after_publication(&document.path);
        }
        Ok(())
    }

    pub(super) fn presentation(&self) -> PlanPresentation {
        let mut documents = Vec::new();
        documents.push(match &self.state {
            Some(state) => PresentedDocument {
                path: state.path.clone(),
                source_generation: Some(state.source_generation),
                target_generation: self.target_generation,
                route: state.route.clone(),
                will_publish: state.needs_publication(),
            },
            None => PresentedDocument {
                path: self.state_path.clone(),
                source_generation: None,
                target_generation: self.target_generation,
                route: Vec::new(),
                will_publish: false,
            },
        });
        documents.extend(self.profiles.iter().map(|profile| PresentedDocument {
            path: profile.path.clone(),
            source_generation: Some(profile.source_generation),
            target_generation: self.target_generation,
            route: profile.route.clone(),
            will_publish: profile.needs_publication(),
        }));
        PlanPresentation {
            configuration_path: self.configuration_path.clone(),
            normal_commands_available: self.target_generation == CURRENT_MANAGED_GENERATION
                || documents
                    .iter()
                    .all(|document| document.source_generation.is_none()),
            documents,
        }
    }

    pub(super) fn build(context: &ResolverContext, target: u32) -> Result<Self, PlanError> {
        let registry = routes();
        let config_path = context.environment_config_path().as_path();
        let configuration_bytes = read_regular(config_path)?;
        let configuration_source = std::str::from_utf8(&configuration_bytes)
            .map_err(|_| PlanError::Invalid("configuration is not UTF-8".to_owned()))?;
        let environment = EnvironmentConfig::parse(configuration_source)
            .map_err(|error| PlanError::Invalid(error.to_string()))?;
        for (store_id, _) in environment.stores() {
            crate::domain::ids::StoreId::parse(store_id.to_owned())
                .map_err(|error| PlanError::Invalid(error.to_string()))?;
        }
        let config_directory = config_path
            .parent()
            .ok_or_else(|| PlanError::UnsafePath(config_path.to_path_buf()))?;
        let mut discovery = Vec::new();
        let mut raw_profiles = BTreeMap::<PathBuf, Vec<u8>>::new();
        for raw in environment.profile_discovery_paths() {
            let declared = bind_discovery(raw, config_directory, context)?;
            let physical = fs::canonicalize(&declared).map_err(|source| PlanError::Io {
                path: declared.clone(),
                source,
            })?;
            let physical = ResolvedPath::from_platform_canonicalized(physical)
                .map_err(|error| PlanError::Invalid(error.to_string()))?
                .into_path_buf();
            require_plain_directory(&declared)?;
            let mut entries = BTreeMap::new();
            for entry in fs::read_dir(&physical).map_err(|source| PlanError::Io {
                path: physical.clone(),
                source,
            })? {
                let path = entry
                    .map_err(|source| PlanError::Io {
                        path: physical.clone(),
                        source,
                    })?
                    .path();
                let kind = entry_kind(&path)?;
                entries.insert(path.clone(), kind);
                if path.extension().and_then(|extension| extension.to_str()) == Some("yaml")
                    && kind == EntryKind::Regular
                {
                    let source = read_regular(&path)?;
                    match raw_profiles.entry(path.clone()) {
                        std::collections::btree_map::Entry::Vacant(slot) => {
                            slot.insert(source);
                        }
                        std::collections::btree_map::Entry::Occupied(_) => {
                            return Err(PlanError::DuplicatePath(path));
                        }
                    }
                }
            }
            discovery.push(DiscoverySnapshot {
                declared_path: declared,
                physical_path: physical,
                entries,
            });
        }

        let state_path = context.state_directory().as_path().join("state.json");
        match fs::symlink_metadata(context.state_directory().as_path()) {
            Ok(_) => require_plain_directory(context.state_directory().as_path())?,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                require_plain_ancestors(context.state_directory().as_path())?;
            }
            Err(source) => {
                return Err(PlanError::Io {
                    path: context.state_directory().as_path().to_path_buf(),
                    source,
                });
            }
        }
        let raw_state = match fs::symlink_metadata(&state_path) {
            Ok(_) => Some(read_regular(&state_path)?),
            Err(source) if source.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(PlanError::Io {
                    path: state_path,
                    source,
                });
            }
        };
        let state_generation = raw_state.as_deref().map(state::state_version).transpose()?;
        let mut profile_ids = BTreeMap::<ProfileId, PathBuf>::new();
        let mut includes = Vec::<(ProfileId, ProfileId)>::new();
        let mut profiles = Vec::new();
        for (path, source) in raw_profiles {
            let (document, id, referenced) = profiles::plan_document(
                path.clone(),
                source,
                state_generation,
                target,
                &registry,
                &environment,
            )?;
            if let Some(previous) = profile_ids.insert(id.clone(), path.clone()) {
                return Err(PlanError::Invalid(format!(
                    "duplicate profile ID {id:?} in {} and {}",
                    previous.display(),
                    path.display()
                )));
            }
            includes.extend(
                referenced
                    .into_iter()
                    .map(|included| (id.clone(), included)),
            );
            profiles.push(document);
        }
        validate_composition(&profile_ids, &includes)?;
        profiles::validate_completed_inventory(
            &profiles,
            target,
            environment.default_profile(),
            context,
        )?;
        if let Some(default_profile) = environment.default_profile() {
            let id = ProfileId::parse(default_profile.to_owned())
                .map_err(|error| PlanError::Invalid(error.to_string()))?;
            if !profile_ids.contains_key(&id) {
                return Err(PlanError::Invalid(format!(
                    "default profile {default_profile:?} is not discovered"
                )));
            }
        }
        let state = raw_state
            .map(|source| {
                let from = state_generation.expect("present state has a version");
                state::plan_document(state_path, source, from, target, &registry)
            })
            .transpose()?;
        Ok(Self {
            target_generation: target,
            configuration_path: config_path.to_path_buf(),
            configuration_bytes,
            state_path: context.state_directory().as_path().join("state.json"),
            discovery,
            state,
            profiles,
        })
    }

    pub(super) fn compare_confirmed(&self, context: &ResolverContext) -> Result<(), PlanError> {
        let refreshed = Self::build(context, self.target_generation)?;
        if self != &refreshed {
            return Err(PlanError::ConfirmedSnapshotChanged);
        }
        Ok(())
    }

    pub(super) fn record_verified_publication(&mut self, path: &Path) -> Result<(), PlanError> {
        let document = if self.state.as_ref().is_some_and(|state| state.path == path) {
            self.state.as_mut().expect("matched state path")
        } else {
            self.profiles
                .iter_mut()
                .find(|profile| profile.path == path)
                .ok_or_else(|| {
                    PlanError::Invalid("document was not in the confirmed plan".to_owned())
                })?
        };
        if !document.needs_publication() || read_regular(path)? != document.candidate {
            return Err(PlanError::ConfirmedSnapshotChanged);
        }
        document.source = document.candidate.clone();
        document.source_generation = document.target_generation;
        document.route.clear();
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn sequence(
        &self,
        mut publish_state: impl FnMut(&PlannedDocument) -> Result<(), PlanError>,
        mut publish_profile: impl FnMut(&PlannedDocument) -> Result<(), PlanError>,
    ) -> Result<(), PlanError> {
        if let Some(state) = self
            .state
            .as_ref()
            .filter(|state| state.needs_publication())
        {
            publish_state(state)?;
        }
        for profile in self
            .profiles
            .iter()
            .filter(|profile| profile.needs_publication())
        {
            publish_profile(profile)?;
        }
        Ok(())
    }
}

fn validate_composition(
    profile_ids: &BTreeMap<ProfileId, PathBuf>,
    includes: &[(ProfileId, ProfileId)],
) -> Result<(), PlanError> {
    let mut adjacency = profile_ids
        .keys()
        .map(|id| (id.clone(), Vec::<ProfileId>::new()))
        .collect::<BTreeMap<_, _>>();
    for (including, included) in includes {
        if !profile_ids.contains_key(included) {
            return Err(PlanError::Invalid(format!(
                "profile {} includes undiscovered profile {}",
                including.as_str(),
                included.as_str()
            )));
        }
        adjacency
            .get_mut(including)
            .expect("discovered profile")
            .push(included.clone());
    }
    fn visit(
        id: &ProfileId,
        adjacency: &BTreeMap<ProfileId, Vec<ProfileId>>,
        pending: &mut BTreeSet<ProfileId>,
        done: &mut BTreeSet<ProfileId>,
    ) -> Result<(), PlanError> {
        if done.contains(id) {
            return Ok(());
        }
        if !pending.insert(id.clone()) {
            return Err(PlanError::Invalid(format!(
                "profile include cycle at {}",
                id.as_str()
            )));
        }
        for included in &adjacency[id] {
            visit(included, adjacency, pending, done)?;
        }
        pending.remove(id);
        done.insert(id.clone());
        Ok(())
    }
    let mut pending = BTreeSet::new();
    let mut done = BTreeSet::new();
    for id in profile_ids.keys() {
        visit(id, &adjacency, &mut pending, &mut done)?;
    }
    Ok(())
}

fn bind_discovery(raw: &str, base: &Path, context: &ResolverContext) -> Result<PathBuf, PlanError> {
    if raw == "~" || (raw.starts_with('~') && !raw.starts_with("~/")) {
        return Err(PlanError::Invalid(format!(
            "invalid profile discovery path {raw:?}"
        )));
    }
    if let Some(suffix) = raw.strip_prefix("~/") {
        return Ok(context.home_directory().as_path().join(suffix));
    }
    let path = Path::new(raw);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}

fn entry_kind(path: &Path) -> Result<EntryKind, PlanError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PlanError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(if is_link_or_reparse_point(&metadata) {
        EntryKind::Link
    } else if metadata.is_file() {
        EntryKind::Regular
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::Other
    })
}

fn read_regular(path: &Path) -> Result<Vec<u8>, PlanError> {
    require_plain_ancestors(path)?;
    if entry_kind(path)? != EntryKind::Regular {
        return Err(PlanError::UnsafePath(path.to_path_buf()));
    }
    fs::read(path).map_err(|source| PlanError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn require_plain_directory(path: &Path) -> Result<(), PlanError> {
    require_plain_ancestors(path)?;
    if entry_kind(path)? != EntryKind::Directory {
        return Err(PlanError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn require_plain_ancestors(path: &Path) -> Result<(), PlanError> {
    for ancestor in path.ancestors().skip(1) {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        let metadata = fs::symlink_metadata(ancestor).map_err(|source| PlanError::Io {
            path: ancestor.to_path_buf(),
            source,
        })?;
        if is_link_or_reparse_point(&metadata) || !metadata.is_dir() {
            return Err(PlanError::UnsafePath(ancestor.to_path_buf()));
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(super) enum PlanError {
    Io { path: PathBuf, source: io::Error },
    UnsafePath(PathBuf),
    DuplicatePath(PathBuf),
    UnsupportedGeneration { from: u32, target: u32 },
    MissingRoute(u32),
    DuplicateRoute(u32),
    InvalidRoute,
    ConfirmedSnapshotChanged,
    Invalid(String),
}

impl fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::UnsafePath(path) => write!(formatter, "unsafe migration path {}", path.display()),
            Self::DuplicatePath(path) => {
                write!(formatter, "duplicate profile path {}", path.display())
            }
            Self::UnsupportedGeneration { from, target } => {
                write!(
                    formatter,
                    "unsupported migration generation {from} to {target}"
                )
            }
            Self::MissingRoute(from) => write!(formatter, "missing migration route from {from}"),
            Self::DuplicateRoute(from) => {
                write!(formatter, "duplicate migration route from {from}")
            }
            Self::InvalidRoute => formatter.write_str("invalid nonadjacent migration route"),
            Self::ConfirmedSnapshotChanged => {
                formatter.write_str("confirmed migration snapshot changed")
            }
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PlanError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        context: ResolverContext,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loadout-continuous-plan-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("profiles")).unwrap();
            fs::create_dir_all(root.join("home")).unwrap();
            fs::write(
                root.join("config.yaml"),
                "schema_version: 2\nprofile_discovery:\n  paths: [profiles]\nstores: {}\n",
            )
            .unwrap();
            let context = ResolverContext::new(
                root.join("home"),
                root.join("runtime.yaml"),
                root.join("config.yaml"),
                root.join("state"),
            )
            .unwrap();
            Self { root, context }
        }

        fn profile(&self, name: &str, generation: u32) {
            fs::write(
                self.root.join("profiles").join(format!("{name}.yaml")),
                format!("schema_version: {generation}\nid: {name}\nresources: {{}}\n"),
            )
            .unwrap();
        }

        fn state(&self, generation: u32) {
            fs::create_dir_all(self.root.join("state")).unwrap();
            let state = match generation {
                3 | 4 => format!(
                    "{{\"schema_version\":{generation},\"resources\":{{}},\"active_operation\":null}}"
                ),
                5 => "{\"schema_version\":5,\"resources\":{},\"applied_lifecycle_topology\":{\"current\":[],\"historical\":[]},\"active_operation\":null}".to_owned(),
                _ => panic!("unsupported fixture generation"),
            };
            fs::write(self.root.join("state/state.json"), state).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn registered_routes_compose_and_invalid_registries_fail_closed() {
        let registry = routes();
        assert_eq!(
            select_routes(&registry, 3, 5)
                .unwrap()
                .iter()
                .map(|route| (route.from, route.to))
                .collect::<Vec<_>>(),
            [(3, 4), (4, 5)]
        );
        assert_eq!(select_routes(&registry, 3, 4).unwrap().len(), 1);
        assert!(matches!(
            select_routes(&registry[1..], 3, 5),
            Err(PlanError::MissingRoute(3))
        ));
        assert!(matches!(
            select_routes(&[registry[0], registry[0]], 3, 4),
            Err(PlanError::DuplicateRoute(3))
        ));
        let invalid = Route {
            to: 3,
            ..registry[0]
        };
        assert!(matches!(
            select_routes(&[invalid], 3, 4),
            Err(PlanError::InvalidRoute)
        ));
        assert!(matches!(
            select_routes(&registry, 5, 4),
            Err(PlanError::UnsupportedGeneration { .. })
        ));
        let future = Route {
            from: 5,
            to: 6,
            ..registry[1]
        };
        assert_eq!(
            select_routes(&[registry[0], registry[1], future], 3, 6)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(select_routes(&registry[1..], 4, 5).unwrap().len(), 1);
    }

    #[test]
    fn facade_plans_mixed_inventory_and_sequences_state_first() {
        let fixture = Fixture::new();
        fixture.state(3);
        fixture.profile("old", 3);
        fixture.profile("middle", 4);
        fixture.profile("new", 5);
        let plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        assert_eq!(plan.state.as_ref().unwrap().route, [(3, 4), (4, 5)]);
        assert_eq!(
            plan.profiles
                .iter()
                .map(|document| (document.source_generation, document.route.len()))
                .collect::<Vec<_>>(),
            [(4, 1), (5, 0), (3, 2)]
        );
        let presentation = plan.presentation();
        assert_eq!(presentation.documents.len(), 4);
        assert!(presentation.normal_commands_available);
        assert_eq!(
            presentation
                .documents
                .iter()
                .filter(|document| document.will_publish)
                .count(),
            3
        );
        let calls = std::cell::RefCell::new(Vec::new());
        plan.sequence(
            |_| {
                calls.borrow_mut().push("state");
                Ok(())
            },
            |_| {
                calls.borrow_mut().push("profile");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*calls.borrow(), ["state", "profile", "profile"]);
    }

    #[test]
    fn absent_state_and_target_four_do_not_create_state_or_plan_downgrades() {
        let fixture = Fixture::new();
        fixture.profile("old", 3);
        fixture.profile("middle", 4);
        let plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 4).unwrap();
        assert!(plan.state.is_none());
        assert_eq!(plan.presentation().documents[0].source_generation, None);
        assert!(!plan.presentation().normal_commands_available);
        assert_eq!(
            plan.profiles
                .iter()
                .filter(|item| item.needs_publication())
                .count(),
            1
        );
        fixture.profile("new", 5);
        assert!(matches!(
            super::super::MigrationFacade::plan_continuous(&fixture.context, 4),
            Err(PlanError::UnsupportedGeneration { .. })
        ));
    }

    #[test]
    fn state_and_profile_generation_matrix_plans_only_remaining_edges() {
        for state_generation in [None, Some(3), Some(4), Some(5)] {
            for profile_generation in [3, 4, 5] {
                let fixture = Fixture::new();
                if let Some(generation) = state_generation {
                    fixture.state(generation);
                }
                fixture.profile("base", profile_generation);
                let plan =
                    super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
                assert_eq!(
                    plan.state.as_ref().map(|state| state.route.len()),
                    state_generation.map(|generation| (5 - generation) as usize)
                );
                assert_eq!(
                    plan.profiles[0].route.len(),
                    (5 - profile_generation) as usize
                );
            }
        }
    }

    #[test]
    fn confirmed_execution_publishes_state_first_and_resumes_mixed_inventory() {
        let fixture = Fixture::new();
        fixture.state(3);
        fixture.profile("old", 3);
        fixture.profile("middle", 4);
        fixture.profile("new", 5);
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        plan.execute_confirmed(&fixture.context).unwrap();
        assert_eq!(
            state::state_version(&fs::read(fixture.root.join("state/state.json")).unwrap())
                .unwrap(),
            5
        );
        for name in ["old", "middle", "new"] {
            let source =
                fs::read_to_string(fixture.root.join("profiles").join(format!("{name}.yaml")))
                    .unwrap();
            assert_eq!(profiles::profile_version(&source).unwrap(), 5);
        }
        let mut resumed =
            super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        assert!(
            resumed
                .profiles
                .iter()
                .all(|item| !item.needs_publication())
        );
        resumed.execute_confirmed(&fixture.context).unwrap();
    }

    #[test]
    fn confirmed_execution_supports_target_four_and_absent_state() {
        let fixture = Fixture::new();
        fixture.profile("old", 3);
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 4).unwrap();
        plan.execute_confirmed(&fixture.context).unwrap();
        assert!(!fixture.root.join("state").exists());
        let source = fs::read_to_string(fixture.root.join("profiles/old.yaml")).unwrap();
        assert_eq!(profiles::profile_version(&source).unwrap(), 4);
        let mut next = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        next.execute_confirmed(&fixture.context).unwrap();
        assert_eq!(
            profiles::profile_version(
                &fs::read_to_string(fixture.root.join("profiles/old.yaml")).unwrap()
            )
            .unwrap(),
            5
        );
    }

    #[test]
    fn state_publication_can_stop_at_schema_four_without_widening_normal_reader() {
        let fixture = Fixture::new();
        fixture.state(3);
        fixture.profile("old", 3);
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 4).unwrap();
        plan.execute_confirmed(&fixture.context).unwrap();
        let bytes = fs::read(fixture.root.join("state/state.json")).unwrap();
        assert_eq!(state::state_version(&bytes).unwrap(), 4);
        assert!(crate::state::validate_current_state_bytes(&bytes).is_err());
        let mut next = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        next.execute_confirmed(&fixture.context).unwrap();
        assert_eq!(
            state::state_version(&fs::read(fixture.root.join("state/state.json")).unwrap())
                .unwrap(),
            5
        );
    }

    #[test]
    fn confirmed_execution_rejects_changed_inventory_before_a_write() {
        let fixture = Fixture::new();
        fixture.state(3);
        fixture.profile("old", 3);
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        fs::write(fixture.root.join("profiles/external.txt"), b"external").unwrap();
        assert!(plan.execute_confirmed(&fixture.context).is_err());
        assert_eq!(
            state::state_version(&fs::read(fixture.root.join("state/state.json")).unwrap())
                .unwrap(),
            3
        );
        assert_eq!(
            profiles::profile_version(
                &fs::read_to_string(fixture.root.join("profiles/old.yaml")).unwrap()
            )
            .unwrap(),
            3
        );
    }

    #[test]
    fn facade_execution_rejects_a_changed_presentation_snapshot_before_lock_or_write() {
        for state_generation in [None, Some(3), Some(5)] {
            let fixture = Fixture::new();
            fixture.profile("old", 3);
            if let Some(generation) = state_generation {
                fixture.state(generation);
            }
            let request = super::super::MigrationRequest::new(fixture.context.clone(), 5);
            let presented = super::super::MigrationFacade::present(&request).unwrap();
            let state_path = fixture.root.join("state/state.json");
            let original_state = fs::read(&state_path).ok();
            let profile_path = fixture.root.join("profiles/old.yaml");
            let original_profile = fs::read(&profile_path).unwrap();
            fs::write(fixture.root.join("profiles/external.txt"), b"external").unwrap();
            let error = super::super::MigrationFacade::execute(request, &presented).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("confirmed migration snapshot changed")
            );
            assert_eq!(fs::read(&state_path).ok(), original_state);
            assert_eq!(fs::read(&profile_path).unwrap(), original_profile);
            assert!(!fixture.root.join("state/state.lock").exists());
        }
    }

    #[test]
    fn state_lock_contention_keeps_all_confirmed_documents_unchanged() {
        let fixture = Fixture::new();
        fixture.state(3);
        fixture.profile("old", 3);
        let repository = crate::state::repository::StateRepository::new(
            fixture.context.state_directory().clone(),
        );
        let _locked = repository.acquire_exclusive_for_migration().unwrap();
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        assert!(plan.execute_confirmed(&fixture.context).is_err());
        assert_eq!(
            state::state_version(&fs::read(fixture.root.join("state/state.json")).unwrap())
                .unwrap(),
            3
        );
        assert_eq!(
            profiles::profile_version(
                &fs::read_to_string(fixture.root.join("profiles/old.yaml")).unwrap()
            )
            .unwrap(),
            3
        );
    }

    #[test]
    fn state_replacement_failure_retains_original_and_stops_profiles() {
        use crate::state::repository::{
            MigrationPublicationFault, fail_migration_publication_at_for_test,
        };

        let fixture = Fixture::new();
        fixture.state(3);
        fixture.profile("old", 3);
        let state_path = fixture.root.join("state/state.json");
        let profile_path = fixture.root.join("profiles/old.yaml");
        let original_state = fs::read(&state_path).unwrap();
        let original_profile = fs::read(&profile_path).unwrap();
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        let _fault = fail_migration_publication_at_for_test(MigrationPublicationFault::Replace);
        let error = plan.execute_confirmed(&fixture.context).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot atomically replace state file")
        );
        assert_eq!(fs::read(&state_path).unwrap(), original_state);
        assert_eq!(fs::read(&profile_path).unwrap(), original_profile);
        let mut resumed =
            super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        resumed.execute_confirmed(&fixture.context).unwrap();
        assert_eq!(
            state::state_version(&fs::read(&state_path).unwrap()).unwrap(),
            5
        );
        assert_eq!(
            profiles::profile_version(&fs::read_to_string(&profile_path).unwrap()).unwrap(),
            5
        );
    }

    #[test]
    fn unverified_state_publication_reports_failure_without_rollback_or_profile_publication() {
        use crate::state::repository::{
            MigrationPublicationFault, fail_migration_publication_at_for_test,
        };

        let fixture = Fixture::new();
        fixture.state(3);
        fixture.profile("old", 3);
        let state_path = fixture.root.join("state/state.json");
        let profile_path = fixture.root.join("profiles/old.yaml");
        let original_profile = fs::read(&profile_path).unwrap();
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        let expected_candidate = plan.state.as_ref().unwrap().candidate.clone();
        let _fault = fail_migration_publication_at_for_test(
            MigrationPublicationFault::PostPublicationValidation,
        );
        let error = plan.execute_confirmed(&fixture.context).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("publication could not be verified")
        );
        assert_eq!(fs::read(&state_path).unwrap(), expected_candidate);
        assert_eq!(fs::read(&profile_path).unwrap(), original_profile);
        assert_eq!(plan.state.as_ref().unwrap().source_generation, 3);
        let mut resumed =
            super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        assert_eq!(resumed.state.as_ref().unwrap().source_generation, 5);
        resumed.execute_confirmed(&fixture.context).unwrap();
        assert_eq!(fs::read(&state_path).unwrap(), expected_candidate);
        assert_eq!(
            profiles::profile_version(&fs::read_to_string(&profile_path).unwrap()).unwrap(),
            5
        );
    }

    #[test]
    fn external_change_after_state_publication_stops_profile_publication_and_resumes() {
        let fixture = Fixture::new();
        fixture.state(3);
        fixture.profile("old", 3);
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        let external = fixture.root.join("profiles/external.txt");
        let state_path = fixture.root.join("state/state.json");
        assert!(
            plan.execute_confirmed_with_hook(&fixture.context, |path| {
                if path == state_path {
                    fs::write(&external, b"external").unwrap();
                }
            })
            .is_err()
        );
        assert_eq!(
            state::state_version(&fs::read(&state_path).unwrap()).unwrap(),
            5
        );
        assert_eq!(
            profiles::profile_version(
                &fs::read_to_string(fixture.root.join("profiles/old.yaml")).unwrap()
            )
            .unwrap(),
            3
        );
        fs::remove_file(&external).unwrap();
        let mut resumed =
            super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        resumed.execute_confirmed(&fixture.context).unwrap();
        assert_eq!(
            profiles::profile_version(
                &fs::read_to_string(fixture.root.join("profiles/old.yaml")).unwrap()
            )
            .unwrap(),
            5
        );
    }

    #[test]
    fn profile_replacement_failure_retains_original_and_later_run_resumes() {
        let fixture = Fixture::new();
        fixture.profile("old", 3);
        let path = fixture.root.join("profiles/old.yaml");
        let original = fs::read(&path).unwrap();
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        profiles::publication::fail_once_at(profiles::publication::PublicationFault::Replace);
        assert!(plan.execute_confirmed(&fixture.context).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        let mut resumed =
            super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        resumed.execute_confirmed(&fixture.context).unwrap();
        assert_eq!(
            profiles::profile_version(&fs::read_to_string(&path).unwrap()).unwrap(),
            5
        );
    }

    #[test]
    fn profile_post_replacement_failure_reports_failure_and_can_resume() {
        let fixture = Fixture::new();
        fixture.profile("old", 3);
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        profiles::publication::fail_once_at(profiles::publication::PublicationFault::AfterReplace);
        assert!(plan.execute_confirmed(&fixture.context).is_err());
        let path = fixture.root.join("profiles/old.yaml");
        assert_eq!(
            profiles::profile_version(&fs::read_to_string(&path).unwrap()).unwrap(),
            5
        );
        let mut resumed =
            super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        resumed.execute_confirmed(&fixture.context).unwrap();
    }

    #[test]
    fn external_change_after_first_profile_stops_remaining_publications() {
        let fixture = Fixture::new();
        fixture.profile("a", 3);
        fixture.profile("b", 3);
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        let first = fixture.root.join("profiles/a.yaml");
        let external = fixture.root.join("profiles/external.txt");
        assert!(
            plan.execute_confirmed_with_hook(&fixture.context, |path| {
                if path == first {
                    fs::write(&external, b"external").unwrap();
                }
            })
            .is_err()
        );
        assert_eq!(
            profiles::profile_version(&fs::read_to_string(&first).unwrap()).unwrap(),
            5
        );
        assert_eq!(
            profiles::profile_version(
                &fs::read_to_string(fixture.root.join("profiles/b.yaml")).unwrap()
            )
            .unwrap(),
            3
        );
    }

    #[test]
    fn active_and_unknown_state_reject_before_publication() {
        let fixture = Fixture::new();
        fixture.profile("base", 3);
        fixture.state(5);
        let path = fixture.root.join("state/state.json");
        let before = fs::read(&path).unwrap();
        let active = String::from_utf8(before.clone())
            .unwrap()
            .replace("\"active_operation\":null", "\"active_operation\":{}");
        fs::write(&path, &active).unwrap();
        assert!(super::super::MigrationFacade::plan_continuous(&fixture.context, 5).is_err());
        assert_eq!(fs::read(&path).unwrap(), active.as_bytes());
        fs::write(
            &path,
            b"{\"schema_version\":6,\"resources\":{},\"active_operation\":null}",
        )
        .unwrap();
        assert!(super::super::MigrationFacade::plan_continuous(&fixture.context, 5).is_err());
    }

    #[test]
    fn unknown_store_and_unsafe_discovery_path_reject_during_preflight() {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("profiles/base.yaml"),
            "schema_version: 5\nid: base\nresources:\n  item:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      target: /tmp/loadout-target\n      source: { store: missing, path: item }\n",
        )
        .unwrap();
        assert!(super::super::MigrationFacade::plan_continuous(&fixture.context, 5).is_err());
        fixture.profile("base", 5);
        #[cfg(unix)]
        {
            let linked = fixture.root.join("linked");
            std::os::unix::fs::symlink(fixture.root.join("profiles"), &linked).unwrap();
            fs::write(
                fixture.root.join("config.yaml"),
                "schema_version: 2\nprofile_discovery:\n  paths: [linked]\nstores: {}\n",
            )
            .unwrap();
            assert!(super::super::MigrationFacade::plan_continuous(&fixture.context, 5).is_err());
        }
    }

    #[test]
    fn incompatible_newer_profile_rejects_before_any_publication() {
        let fixture = Fixture::new();
        fixture.state(3);
        fs::write(
            fixture.root.join("profiles/new.yaml"),
            "schema_version: 5\nid: new\nstages: { build: { after: [] } }\nresources: {}\n",
        )
        .unwrap();
        assert!(super::super::MigrationFacade::plan_continuous(&fixture.context, 5).is_err());
        assert_eq!(
            fs::read(fixture.root.join("state/state.json")).unwrap()[0],
            b'{'
        );
    }

    #[test]
    fn continuous_route_accepts_schema_four_parameters() {
        let fixture = Fixture::new();
        fixture.state(4);
        let source =
            "schema_version: 4\nid: themed\nparameters: { theme: string }\nresources: {}\n";
        fs::write(fixture.root.join("profiles/themed.yaml"), source).unwrap();
        let plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        assert_eq!(
            plan.profiles[0].candidate,
            source
                .replacen("schema_version: 4", "schema_version: 5", 1)
                .into_bytes()
        );
    }

    #[test]
    fn completed_inventory_checks_typed_include_bindings_before_publication() {
        let fixture = Fixture::new();
        fixture.state(4);
        fs::write(
            fixture.root.join("profiles/themed.yaml"),
            "schema_version: 4\nid: themed\nparameters: { theme: string }\nresources: {}\n",
        )
        .unwrap();
        let root = fixture.root.join("profiles/root.yaml");
        fs::write(
            &root,
            "schema_version: 4\nid: root\nincludes: [{ id: themed, as: themed-for-root, with: { theme: blue } }]\nresources: {}\n",
        )
        .unwrap();
        assert!(super::super::MigrationFacade::plan_continuous(&fixture.context, 5).is_ok());
        fs::write(
            &root,
            "schema_version: 4\nid: root\nincludes: [{ id: themed, as: themed-for-root, with: { theme: 7 } }]\nresources: {}\n",
        )
        .unwrap();
        assert!(super::super::MigrationFacade::plan_continuous(&fixture.context, 5).is_err());
        fs::write(
            &root,
            "schema_version: 4\nid: root\nincludes: [{ id: themed, with: { theme: blue } }]\nresources: {}\n",
        )
        .unwrap();
        assert!(super::super::MigrationFacade::plan_continuous(&fixture.context, 5).is_err());
    }

    #[test]
    fn completed_inventory_rejects_invalid_orchestration_and_target_collisions() {
        let fixture = Fixture::new();
        fixture.profile("old", 3);
        fs::write(
            fixture.root.join("config.yaml"),
            "schema_version: 2\nprofile_discovery:\n  paths: [profiles]\nstores:\n  dotfiles:\n    type: local\n    properties: { path: store }\n",
        )
        .unwrap();
        let path = fixture.root.join("profiles/new.yaml");
        let prefix = "schema_version: 5\nid: new\n";
        let resource = "    type: file\n    properties:\n      kind: file\n      operation: link\n      source: { store: dotfiles, path: item }\n      target: ~/same\n";
        for (body, expected) in [
            (
                "stages: { build: { after: [missing] } }\nresources: {}\n".to_owned(),
                "undeclared local stage",
            ),
            (
                format!("resources:\n  item:\n    stage: missing\n{resource}"),
                "undeclared local stage",
            ),
            (
                format!("resources:\n  item:\n    depends_on: [missing]\n{resource}"),
                "undeclared local resource",
            ),
            (
                format!("resources:\n  first:\n{resource}  second:\n{resource}"),
                "claimed by both",
            ),
        ] {
            let source = format!("{prefix}{body}");
            fs::write(&path, &source).unwrap();
            let error =
                super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
            assert_eq!(fs::read(&path).unwrap(), source.as_bytes());
            assert_eq!(
                fs::read(fixture.root.join("profiles/old.yaml")).unwrap(),
                b"schema_version: 3\nid: old\nresources: {}\n"
            );
        }
    }

    #[test]
    fn completed_inventory_rejects_qualified_references_and_bound_target_collisions() {
        let fixture = Fixture::new();
        fixture.profile("old", 3);
        fs::write(
            fixture.root.join("config.yaml"),
            "schema_version: 2\nprofile_discovery:\n  paths: [profiles]\nstores:\n  dotfiles:\n    type: local\n    properties: { path: store }\n",
        )
        .unwrap();
        let child = fixture.root.join("profiles/child.yaml");
        fs::write(
            &child,
            "schema_version: 5\nid: child\nparameters: { destination: string }\nstages: { build: { after: [] } }\nresources:\n  item:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source: { store: dotfiles, path: item }\n      target: { parameter: destination }\n",
        )
        .unwrap();
        let root = fixture.root.join("profiles/root.yaml");
        fs::write(
            &root,
            "schema_version: 5\nid: root\nincludes: [{ id: child, as: first, with: { destination: '~/one' } }]\nstages: { deploy: { after: [root/first/missing] } }\nresources: {}\n",
        )
        .unwrap();
        let error =
            super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not name a resolved stage instance")
        );
        fs::write(
            &root,
            "schema_version: 5\nid: root\nincludes:\n  - { id: child, as: first, with: { destination: '~/same' } }\n  - { id: child, as: second, with: { destination: '~/same' } }\nresources: {}\n",
        )
        .unwrap();
        let error =
            super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap_err();
        assert!(error.to_string().contains("claimed by both"));
        assert_eq!(
            fs::read(fixture.root.join("profiles/old.yaml")).unwrap(),
            b"schema_version: 3\nid: old\nresources: {}\n"
        );
    }

    #[test]
    fn confirmed_snapshot_detects_config_entry_profile_and_state_changes() {
        let fixture = Fixture::new();
        fixture.state(3);
        fixture.profile("old", 3);
        let plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        plan.compare_confirmed(&fixture.context).unwrap();
        fs::write(fixture.root.join("profiles/extra.txt"), b"new entry").unwrap();
        assert!(matches!(
            plan.compare_confirmed(&fixture.context),
            Err(PlanError::ConfirmedSnapshotChanged)
        ));
        fs::remove_file(fixture.root.join("profiles/extra.txt")).unwrap();
        fs::write(
            fixture.root.join("profiles/old.yaml"),
            "schema_version: 3\nid: old\nresources: {}\n# changed\n",
        )
        .unwrap();
        assert!(matches!(
            plan.compare_confirmed(&fixture.context),
            Err(PlanError::ConfirmedSnapshotChanged)
        ));
        fixture.profile("old", 3);
        fs::write(
            fixture.root.join("state/state.json"),
            b"{\"schema_version\":3,\"resources\":{},\"active_operation\":null}\n",
        )
        .unwrap();
        assert!(matches!(
            plan.compare_confirmed(&fixture.context),
            Err(PlanError::ConfirmedSnapshotChanged)
        ));
        fixture.state(3);
        fs::write(
            fixture.root.join("config.yaml"),
            "schema_version: 2\nprofile_discovery:\n  paths: [profiles]\nstores: {}\n# changed\n",
        )
        .unwrap();
        assert!(matches!(
            plan.compare_confirmed(&fixture.context),
            Err(PlanError::ConfirmedSnapshotChanged)
        ));
    }

    #[test]
    fn confirmed_snapshot_checks_absent_and_current_state_as_well() {
        let absent = Fixture::new();
        absent.profile("base", 5);
        let plan = super::super::MigrationFacade::plan_continuous(&absent.context, 5).unwrap();
        absent.state(5);
        assert!(matches!(
            plan.compare_confirmed(&absent.context),
            Err(PlanError::ConfirmedSnapshotChanged)
        ));

        let current = Fixture::new();
        current.state(5);
        current.profile("base", 5);
        let plan = super::super::MigrationFacade::plan_continuous(&current.context, 5).unwrap();
        fs::write(current.root.join("profiles/new.txt"), b"new entry").unwrap();
        assert!(matches!(
            plan.compare_confirmed(&current.context),
            Err(PlanError::ConfirmedSnapshotChanged)
        ));
    }

    #[test]
    fn only_verified_self_publications_advance_the_expected_snapshot() {
        let fixture = Fixture::new();
        fixture.state(3);
        fixture.profile("old", 3);
        let mut plan = super::super::MigrationFacade::plan_continuous(&fixture.context, 5).unwrap();
        let state = plan.state.as_ref().unwrap().clone();
        assert!(matches!(
            plan.record_verified_publication(&state.path),
            Err(PlanError::ConfirmedSnapshotChanged)
        ));
        fs::write(&state.path, &state.candidate).unwrap();
        plan.record_verified_publication(&state.path).unwrap();
        plan.compare_confirmed(&fixture.context).unwrap();

        let profile = plan.profiles[0].clone();
        fs::write(&profile.path, &profile.candidate).unwrap();
        plan.record_verified_publication(&profile.path).unwrap();
        plan.compare_confirmed(&fixture.context).unwrap();
        assert!(matches!(
            plan.record_verified_publication(&profile.path),
            Err(PlanError::ConfirmedSnapshotChanged)
        ));
    }
}
