//! Structural guard for the migration facade boundary.
//!
//! The guard works on `syn` syntax trees, not source text. Its deliberately finite local name
//! resolution follows `use` aliases and `let` bindings only far enough to reject attempts to
//! hide facade dispatch or route capabilities behind a local name.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use proc_macro2::TokenTree;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprPath, File, ImplItem, Item, ItemFn, ItemMod, ItemUse, Local, Meta, Pat,
    Path as SynPath, StmtMacro, TraitItem, UseTree,
};

const NATIVE_TARGETS: [&str; 3] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

static NEXT_INVENTORY_FIXTURE: AtomicU64 = AtomicU64::new(0);

/// A captured, finite configuration record.  Nothing in this type is derived from the
/// test process's target: each fact comes from `rustc --target <triple> --print cfg`.
#[derive(Debug)]
struct TargetRecord {
    triple: &'static str,
    rustc_version: String,
    /// The compiler output is evidence, not a lossy input to the predicate evaluator.
    /// In particular, rustc may emit several values for one key such as `target_feature`.
    raw_cfg: String,
    facts: CfgFacts,
    roots: Vec<PathBuf>,
}

/// The finite cfg fact set for one exact target record.
///
/// `values` deliberately keeps every line in order. It is not a map from a cfg name to one
/// value because collapsing `target_feature` (or another repeated fact) changes cfg semantics.
#[derive(Clone, Debug)]
struct CfgFacts {
    values: BTreeMap<String, Vec<Option<String>>>,
    known_values: BTreeMap<String, BTreeSet<Option<String>>>,
    test_enabled: bool,
    features: BTreeSet<String>,
}

impl CfgFacts {
    fn from_pairs(pairs: impl IntoIterator<Item = (String, Option<String>)>) -> Self {
        let mut values = BTreeMap::new();
        for (name, value) in pairs {
            values.entry(name).or_insert_with(Vec::new).push(value);
        }
        Self {
            known_values: values
                .iter()
                .map(|(name, values)| (name.clone(), values.iter().cloned().collect()))
                .collect(),
            values,
            test_enabled: false,
            features: BTreeSet::new(),
        }
    }

    fn has_bare(&self, name: &str) -> bool {
        if name == "test" {
            return self.test_enabled;
        }
        self.values
            .get(name)
            .is_some_and(|values| values.iter().any(Option::is_none))
    }

    fn has_value(&self, name: &str, expected: &str) -> Result<bool, String> {
        if name == "feature" {
            return self
                .features
                .contains(expected)
                .then_some(true)
                .ok_or_else(|| "unknown cfg predicate".to_owned());
        }
        if !self.knows_value(name, expected) {
            return Err("unknown cfg predicate".to_owned());
        }
        let values = self.values.get(name).map(Vec::as_slice).unwrap_or(&[]);
        Ok(values
            .iter()
            .any(|value| value.as_deref() == Some(expected)))
    }

    fn knows_name(&self, name: &str) -> bool {
        matches!(name, "test" | "feature" | "unix" | "windows") || self.values.contains_key(name)
    }

    fn knows_value(&self, name: &str, expected: &str) -> bool {
        self.known_values
            .get(name)
            .is_some_and(|values| values.contains(&Some(expected.to_owned())))
    }
}

/// The complete stage-one result for one compiler configuration.  Stage two deliberately accepts
/// this finite inventory instead of walking `src` again, so an orphan or a source loaded through
/// an unsupported mechanism cannot be silently omitted from the structural boundary check.
#[derive(Debug)]
struct SourceInventory {
    target: &'static str,
    facts: CfgFacts,
    roots: Vec<PathBuf>,
    active: BTreeMap<PathBuf, InventorySource>,
    inactive: BTreeMap<PathBuf, InventorySource>,
}

#[derive(Debug)]
struct InventorySource {
    root: PathBuf,
    module_path: Vec<String>,
    parent_edge: Option<String>,
    cfg_selection: CfgSelection,
    source: String,
}

struct InactiveSource {
    source_path: PathBuf,
    child_directory: PathBuf,
    module_path: Vec<String>,
    parent_edge: Option<String>,
    cfg_selection: CfgSelection,
}

struct InactiveWalk<'a> {
    root: &'a Path,
    inventory: &'a mut SourceInventory,
    declared: &'a mut BTreeSet<PathBuf>,
}

/// The exact conditional decision that admitted or excluded one source for a target record.
#[derive(Debug)]
struct CfgSelection {
    predicates: Vec<(String, bool)>,
    selected: bool,
    exclusion: Option<String>,
}

/// Stage one of the migration isolation guard deliberately obtains its facts from the selected
/// compiler target, never from the host process or the test crate's `cfg!` values.
#[test]
fn three_target_inventory_selects_only_the_conventional_executor_edges() {
    let records = exact_target_records();
    let executor = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/executor/mod.rs");
    let source = fs::read_to_string(&executor).expect("executor module must be readable");
    let file = syn::parse_file(&source).expect("executor module must parse");
    assert!(file.items.iter().all(|item| !matches!(item, Item::Mod(module) if module.attrs.iter().any(|attribute| attribute.path().is_ident("path")))), "executor module declarations must be conventional");

    for record in records {
        assert!(
            record.roots.iter().any(|root| root.ends_with("src/lib.rs")),
            "{}",
            record.triple
        );
        assert!(
            record
                .roots
                .iter()
                .any(|root| root.ends_with("src/main.rs")),
            "{}",
            record.triple
        );
        assert!(!record.rustc_version.is_empty(), "{}", record.triple);
        let edges = active_external_module_edges(&file, &record.facts);
        let reexports = active_directory_link_reexports(&file, &record.facts);
        println!(
            "target={} rustc={} raw_cfg:\n{}roots={:?}",
            record.triple,
            record.rustc_version.lines().next().unwrap_or("unknown"),
            record.raw_cfg,
            record.roots
        );
        match record.triple {
            "x86_64-unknown-linux-gnu" | "aarch64-apple-darwin" => {
                assert_eq!(
                    edges,
                    vec![(
                        "directory_link".to_owned(),
                        PathBuf::from("directory_link.rs")
                    )],
                    "{}",
                    record.triple
                );
                assert!(reexports.is_empty(), "{}", record.triple);
            }
            "x86_64-pc-windows-msvc" => {
                assert_eq!(
                    edges,
                    vec![(
                        "directory_link_windows".to_owned(),
                        PathBuf::from("directory_link_windows.rs")
                    )]
                );
                assert_eq!(
                    reexports,
                    vec![(
                        "directory_link_windows".to_owned(),
                        "directory_link".to_owned()
                    )]
                );
            }
            _ => unreachable!(),
        }
    }
}

/// These controls make the inventory rules falsifiable without depending on a host target.
#[test]
fn three_target_inventory_rejects_missing_extra_inactive_and_path_redirected_executor_edges() {
    let windows = exact_target_records()
        .into_iter()
        .find(|record| record.triple == "x86_64-pc-windows-msvc")
        .expect("Windows record")
        .facts;
    for (name, source) in [
        (
            "inventory_missing_windows_declaration",
            "#[cfg(windows)] pub(crate) use directory_link_windows as directory_link;",
        ),
        (
            "inventory_inactive_windows_declaration",
            "#[cfg(unix)] mod directory_link_windows; #[cfg(windows)] pub(crate) use directory_link_windows as directory_link;",
        ),
        (
            "inventory_extra_windows_declaration",
            "#[cfg(windows)] mod directory_link_windows; #[cfg(windows)] mod directory_link; #[cfg(windows)] pub(crate) use directory_link_windows as directory_link;",
        ),
        (
            "inventory_path_redirected_windows_declaration",
            "#[cfg(windows)] #[path = \"other.rs\"] mod directory_link_windows; #[cfg(windows)] pub(crate) use directory_link_windows as directory_link;",
        ),
        (
            "inventory_missing_windows_reexport",
            "#[cfg(windows)] mod directory_link_windows;",
        ),
        (
            "inventory_inactive_windows_reexport",
            "#[cfg(windows)] mod directory_link_windows; #[cfg(unix)] pub(crate) use directory_link_windows as directory_link;",
        ),
        (
            "inventory_extra_windows_reexport",
            "#[cfg(windows)] mod directory_link_windows; #[cfg(windows)] pub(crate) use directory_link_windows as directory_link; #[cfg(windows)] pub(crate) use directory_link_windows as alternate;",
        ),
        (
            "inventory_path_selected_module",
            "#[cfg(windows)] #[path = \"directory_link_windows.rs\"] mod directory_link_windows; #[cfg(windows)] pub(crate) use directory_link_windows as directory_link;",
        ),
    ] {
        let file = syn::parse_file(source).unwrap();
        assert!(
            validate_windows_executor_inventory(&file, &windows).is_err(),
            "{name}"
        );
    }
}

/// Named Unix controls keep an inactive complementary record from accidentally satisfying an
/// executor edge assertion.  They deliberately use the Linux captured record, not `cfg!(unix)`.
#[test]
fn inventory_unix_controls_reject_missing_extra_inactive_and_path_redirected_edges() {
    let linux = exact_target_records()
        .into_iter()
        .find(|record| record.triple == "x86_64-unknown-linux-gnu")
        .expect("Linux record")
        .facts;
    for (name, source) in [
        (
            "inventory_missing_unix_declaration",
            "#[cfg(windows)] mod directory_link;",
        ),
        (
            "inventory_inactive_unix_declaration",
            "#[cfg(windows)] mod directory_link;",
        ),
        (
            "inventory_extra_unix_declaration",
            "#[cfg(unix)] mod directory_link; #[cfg(unix)] mod directory_link_windows;",
        ),
        (
            "inventory_path_redirected_unix_declaration",
            "#[cfg(unix)] #[path = \"other.rs\"] mod directory_link;",
        ),
        (
            "inventory_inline_module",
            "#[cfg(unix)] mod directory_link {}",
        ),
    ] {
        let file = syn::parse_file(source).unwrap();
        assert!(
            validate_unix_executor_inventory(&file, &linux).is_err(),
            "{name}"
        );
    }
}

fn cargo_selected_production_roots() -> Result<Vec<PathBuf>, String> {
    cargo_selected_production_roots_for(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("Cargo.toml")
            .as_path(),
    )
}

#[cfg(windows)]
fn normal_existing_path(path: &Path) -> Result<PathBuf, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("path must be readable: {error}"))?;
    let raw = canonical
        .to_str()
        .ok_or_else(|| "path must be valid Unicode".to_owned())?;
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        Ok(PathBuf::from(format!(r"\\{unc}")))
    } else {
        Ok(PathBuf::from(raw.strip_prefix(r"\\?\").unwrap_or(raw)))
    }
}

#[cfg(not(windows))]
fn normal_existing_path(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("path must be readable: {error}"))
}

fn cargo_selected_production_roots_for(manifest_path: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--no-deps",
            "--format-version=1",
            "--manifest-path",
        ])
        .arg(manifest_path)
        .output()
        .map_err(|error| format!("Cargo metadata failed to start: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Cargo metadata must be JSON: {error}"))?;
    let manifest_path = normal_existing_path(manifest_path)
        .map_err(|error| format!("Cargo manifest path must be readable: {error}"))?;
    let package = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| {
            package["manifest_path"]
                .as_str()
                .and_then(|path| normal_existing_path(Path::new(path)).ok())
                .as_ref()
                == Some(&manifest_path)
        })
        .ok_or_else(|| "Cargo metadata root package must be present".to_owned())?;
    if package["name"].as_str() != Some("loadout") {
        return Err("metadata manifest disagreement: unexpected production package".to_owned());
    }
    let roots = package["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|target| -> Result<Option<(String, PathBuf)>, String> {
            let kinds = target["kind"]
                .as_array()
                .ok_or_else(|| "Cargo metadata production target is malformed".to_owned())?;
            let kind = kinds
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>();
            if kind
                .iter()
                .any(|kind| matches!(*kind, "test" | "example" | "bench" | "custom-build"))
            {
                return Ok(None);
            }
            if kind.len() != 1 || !matches!(kind[0], "lib" | "bin") {
                return Err(format!("unmodelled Cargo target kind: {kind:?}"));
            }
            let path = target["src_path"]
                .as_str()
                .ok_or_else(|| "Cargo metadata production target is malformed".to_owned())?;
            Ok(Some((kind[0].to_owned(), PathBuf::from(path))))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let expected = [
        (
            "lib".to_owned(),
            manifest_path.parent().unwrap().join("src/lib.rs"),
        ),
        (
            "bin".to_owned(),
            manifest_path.parent().unwrap().join("src/main.rs"),
        ),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let selected = roots.iter().cloned().collect::<BTreeSet<_>>();
    if selected != expected || selected.len() != roots.len() {
        return Err("metadata manifest disagreement: production target policy".to_owned());
    }
    Ok(roots.into_iter().map(|(_, root)| root).collect())
}

fn exact_target_records() -> Vec<TargetRecord> {
    assert!(
        std::env::var_os("RUSTFLAGS").is_none(),
        "ambient RUSTFLAGS are not an admitted cfg record"
    );
    assert!(
        std::env::var_os("CARGO_ENCODED_RUSTFLAGS").is_none(),
        "ambient CARGO_ENCODED_RUSTFLAGS are not an admitted cfg record"
    );
    let roots = cargo_selected_production_roots()
        .expect("production target policy must admit Cargo metadata");
    let mut records = NATIVE_TARGETS
        .into_iter()
        .map(|triple| {
            let (raw_cfg, facts) = compiler_cfg(triple);
            TargetRecord {
                triple,
                rustc_version: compiler_version(triple),
                raw_cfg,
                facts,
                roots: roots.clone(),
            }
        })
        .collect::<Vec<_>>();
    let known_values = records.iter().fold(BTreeMap::new(), |mut all, record| {
        for (name, values) in &record.facts.values {
            all.entry(name.clone())
                .or_insert_with(BTreeSet::new)
                .extend(values.iter().cloned());
        }
        all
    });
    for record in &mut records {
        record.facts.known_values = known_values.clone();
    }
    records
}

fn compiler_version(target: &str) -> String {
    let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--target", target, "-vV"])
        .output()
        .expect("rustc -vV must run");
    assert!(
        output.status.success(),
        "rustc -vV failed for {target}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("rustc -vV must be UTF-8")
}

fn compiler_cfg(target: &str) -> (String, CfgFacts) {
    let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--target", target, "--print", "cfg"])
        .output()
        .expect("rustc must run");
    assert!(
        output.status.success(),
        "rustc --print cfg failed for {target}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = String::from_utf8(output.stdout).expect("rustc --print cfg must be UTF-8");
    let facts = CfgFacts::from_pairs(raw.lines().map(|line| match line.split_once('=') {
        Some((name, value)) => (name.to_owned(), Some(value.trim_matches('"').to_owned())),
        None => (line.to_owned(), None),
    }));
    (raw, facts)
}

#[test]
fn stage_one_inventory_precedes_stage_two_for_every_exact_target_record() {
    for record in exact_target_records() {
        let inventory = build_source_inventory(&record).unwrap_or_else(|error| {
            panic!(
                "stage 1 failed before stage 2 for {}: {error}",
                record.triple
            )
        });
        assert_eq!(inventory.target, record.triple);
        assert!(
            inventory
                .active
                .keys()
                .any(|path| path.ends_with("src/lib.rs"))
        );
        assert!(
            inventory
                .active
                .keys()
                .any(|path| path.ends_with("src/main.rs"))
        );
        assert!(
            inventory
                .active
                .keys()
                .any(|path| path.ends_with("src/migration/mod.rs"))
        );
        assert_executor_inventory(&inventory, record.triple);
        // Stage two is intentionally fed only this recorded set, never a recursive source scan.
        let violations = scan_inventory_sources(&inventory);
        assert!(
            violations.is_empty(),
            "{}: {}",
            record.triple,
            violations.join("\n")
        );
        report_stage_one_inventory(&inventory);
    }
}

#[test]
fn stage_one_named_inventory_controls_fail_closed_before_stage_two() {
    for (name, source, children, expected) in [
        (
            "inventory_unmodelled_cfg_predicate",
            "#[cfg(feature = \"future\")] mod child;",
            &[][..],
            "unknown cfg predicate",
        ),
        (
            "inventory_unmodelled_cfg_configuration",
            "#[cfg(target_os = \"plan9\")] mod child;",
            &[][..],
            "unknown cfg predicate",
        ),
        (
            "inventory_unseen_conditional_module",
            "#[cfg(unix)] mod child;",
            &[][..],
            "missing or ambiguous child",
        ),
        (
            "inventory_inline_module",
            "mod child {}",
            &[][..],
            "inline module",
        ),
        (
            "inventory_path_selected_module",
            "#[path = \"child.rs\"] mod child;",
            &[("child.rs", "")][..],
            "path redirected",
        ),
        (
            "inventory_include_generated_module",
            "include!(\"child.rs\");",
            &[("child.rs", "")][..],
            "source loading",
        ),
        (
            "inventory_macro_module",
            "make_module! { mod child; }",
            &[][..],
            "source loading",
        ),
        (
            "inventory_ambiguous_or_missing_child",
            "mod child;",
            &[("child.rs", ""), ("child/mod.rs", "")][..],
            "missing or ambiguous child",
        ),
    ] {
        assert_inventory_fixture(name, source, children, expected);
    }
    let fixture = InventoryFixture::new();
    let source_root = fixture.root.join("src");
    fs::create_dir_all(&source_root).expect("fixture source root");
    let root = source_root.join("lib.rs");
    fs::write(&root, "mod declared;").expect("fixture root");
    fs::write(source_root.join("declared.rs"), "").expect("fixture child");
    fs::write(source_root.join("orphan.rs"), "").expect("fixture orphan");
    let facts = CfgFacts::from_pairs([("unix".to_owned(), None)]);
    let record = TargetRecord {
        triple: NATIVE_TARGETS[0],
        rustc_version: "fixture".to_owned(),
        raw_cfg: "unix".to_owned(),
        facts: facts.clone(),
        roots: vec![root.clone()],
    };
    assert!(matches!(
        build_source_inventory_for_roots(&record, &source_root, std::slice::from_ref(&root)),
        Err(rule) if rule.contains("orphan/unclassified source")
    ));

    fs::remove_file(source_root.join("orphan.rs")).expect("remove fixture orphan");
    let unsupported = TargetRecord {
        triple: "fixture-unsupported-target",
        rustc_version: "fixture".to_owned(),
        raw_cfg: "unix".to_owned(),
        facts,
        roots: vec![root.clone()],
    };
    assert!(matches!(
        build_source_inventory_for_roots(&unsupported, &source_root, std::slice::from_ref(&root)),
        Err(rule) if rule.contains("unsupported target")
    ));

    let outside_root = fixture.root.join("metadata-outside-source.rs");
    fs::write(&outside_root, "").expect("fixture metadata root");
    assert!(matches!(
        build_source_inventory_for_roots(&record, &source_root, &[outside_root]),
        Err(rule) if rule.contains("metadata root is outside source inventory")
    ));

    assert_cargo_metadata_fixture(
        "inventory_extra_cargo_target",
        "[package]\nname = \"loadout\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"loadout\"\npath = \"src/main.rs\"\n\n[[bin]]\nname = \"extra\"\npath = \"src/extra.rs\"\n",
        &["src/lib.rs", "src/main.rs", "src/extra.rs"],
    );
    assert_cargo_metadata_fixture(
        "inventory_metadata_manifest_disagreement",
        "[package]\nname = \"loadout\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/not-the-metadata-root.rs\"\n\n[[bin]]\nname = \"loadout\"\npath = \"src/main.rs\"\n",
        &["src/not-the-metadata-root.rs", "src/main.rs"],
    );
}

/// These fixtures exercise cfg_attr through the production Stage-1 source builder without invoking Stage 2, so cfg_attr selection and rejection cannot be masked by structural scanning.
#[test]
fn stage_one_cfg_attr_inventory_fixtures_select_inactivate_and_fail_closed_before_stage_two() {
    let selected = build_inventory_fixture(
        "cfg_attr_selected",
        "#[cfg_attr(unix, cfg(unix))] mod child;",
        &[("child.rs", "")],
    )
    .expect("selected cfg_attr fixture must pass Stage 1");
    let selected_child = selected.roots[0].parent().unwrap().join("child.rs");
    assert!(selected.active.contains_key(&selected_child));
    assert!(!selected.inactive.contains_key(&selected_child));
    assert!(selected.active[&selected_child].cfg_selection.selected);

    let inactive = build_inventory_fixture(
        "cfg_attr_inactive",
        "#[cfg_attr(unix, cfg(windows))] mod child;",
        &[("child.rs", "")],
    )
    .expect("inactive cfg_attr fixture must pass Stage 1");
    let inactive_child = inactive.roots[0].parent().unwrap().join("child.rs");
    assert!(!inactive.active.contains_key(&inactive_child));
    assert!(inactive.inactive.contains_key(&inactive_child));
    assert!(!inactive.inactive[&inactive_child].cfg_selection.selected);

    assert_inventory_fixture(
        "cfg_attr_error",
        "#[cfg_attr(unix, cfg(feature = \"unmodelled\"))] mod child;",
        &[],
        "unknown cfg predicate",
    );
    assert_inventory_fixture(
        "cfg_attr_unsupported_result",
        "#[cfg_attr(unix, allow(dead_code))] mod child;",
        &[],
        "unsupported cfg_attr result",
    );
}

fn build_inventory_fixture(
    name: &str,
    root_source: &str,
    children: &[(&str, &str)],
) -> Result<SourceInventory, String> {
    let fixture = InventoryFixture::new();
    let source_root = fixture.root.join("src");
    fs::create_dir_all(&source_root).expect("fixture source root");
    let root = source_root.join("lib.rs");
    fs::write(&root, root_source).expect("fixture root");
    for (relative, source) in children {
        let path = source_root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture child parent"))
            .expect("fixture child parent");
        fs::write(path, source).expect("fixture child");
    }
    let record = inventory_fixture_record(root.clone());
    build_source_inventory_for_roots(&record, &source_root, &[root])
        .map_err(|error| format!("{name}: {error}"))
}

fn assert_inventory_fixture(
    name: &str,
    root_source: &str,
    children: &[(&str, &str)],
    expected: &str,
) {
    assert!(
        matches!(
            build_inventory_fixture(name, root_source, children),
            Err(rule) if rule.contains(expected)
        ),
        "{name} must fail through the real stage-one inventory path"
    );
}

fn inventory_fixture_record(root: PathBuf) -> TargetRecord {
    TargetRecord {
        triple: NATIVE_TARGETS[0],
        rustc_version: "fixture".to_owned(),
        raw_cfg: "unix".to_owned(),
        facts: CfgFacts::from_pairs([("unix".to_owned(), None)]),
        roots: vec![root],
    }
}

fn assert_cargo_metadata_fixture(name: &str, manifest: &str, sources: &[&str]) {
    let fixture = InventoryFixture::new();
    let manifest_path = fixture.root.join("Cargo.toml");
    let source_root = fixture.root.join("src");
    fs::create_dir_all(&source_root).expect("Cargo fixture source root");
    fs::write(&manifest_path, manifest).expect("Cargo fixture manifest");
    for source in sources {
        fs::write(source_root.join(source.trim_start_matches("src/")), "")
            .expect("Cargo fixture source");
    }
    let record = inventory_fixture_record(source_root.join("lib.rs"));
    assert!(
        build_source_inventory_for_manifest(&record, &manifest_path).is_err(),
        "{name} must fail through the production stage-one admission path before stage two"
    );
}

fn build_source_inventory(record: &TargetRecord) -> Result<SourceInventory, String> {
    build_source_inventory_for_manifest(
        record,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
}

fn build_source_inventory_for_manifest(
    record: &TargetRecord,
    manifest_path: &Path,
) -> Result<SourceInventory, String> {
    let roots = cargo_selected_production_roots_for(manifest_path)?;
    if roots != record.roots {
        return Err("metadata manifest disagreement: admitted target roots".to_owned());
    }
    let source_root = manifest_path
        .parent()
        .ok_or_else(|| "Cargo manifest has no parent".to_owned())?
        .join("src");
    build_source_inventory_for_roots(record, &source_root, &roots)
}

fn build_source_inventory_for_roots(
    record: &TargetRecord,
    source_root: &Path,
    roots: &[PathBuf],
) -> Result<SourceInventory, String> {
    if !NATIVE_TARGETS.contains(&record.triple) {
        return Err(format!("stage 1 {}: unsupported target", record.triple));
    }
    let mut inventory = SourceInventory {
        target: record.triple,
        facts: record.facts.clone(),
        roots: roots.to_vec(),
        active: BTreeMap::new(),
        inactive: BTreeMap::new(),
    };
    let mut declared = BTreeSet::new();
    for root in roots {
        if !root.starts_with(source_root)
            || root.extension().and_then(|value| value.to_str()) != Some("rs")
        {
            return Err(format!(
                "stage 1 {}: metadata root is outside source inventory: {}",
                record.triple,
                root.display()
            ));
        }
        inventory_walk(
            root,
            module_directory(root),
            Vec::new(),
            None,
            root,
            CfgSelection::unconditional(),
            &record.facts,
            &mut inventory,
            &mut declared,
            &mut BTreeSet::new(),
        )?;
    }
    let physical = rust_sources(source_root)?;
    for path in physical {
        if !declared.contains(&path) {
            return Err(format!(
                "stage 1 {}: orphan/unclassified source {}",
                record.triple,
                path.display()
            ));
        }
    }
    Ok(inventory)
}

fn module_directory(source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if stem == "mod" || stem == "lib" || stem == "main" {
        source.parent().expect("source parent").to_path_buf()
    } else {
        source.with_extension("")
    }
}

#[allow(clippy::too_many_arguments)]
fn inventory_walk(
    source_path: &Path,
    child_directory: PathBuf,
    module_path: Vec<String>,
    parent_edge: Option<String>,
    root: &Path,
    cfg_selection: CfgSelection,
    facts: &CfgFacts,
    inventory: &mut SourceInventory,
    declared: &mut BTreeSet<PathBuf>,
    visiting: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    let canonical = source_path.to_path_buf();
    if !declared.insert(canonical.clone()) {
        if inventory.active.contains_key(&canonical) {
            return Err(format!(
                "stage 1 {}: duplicate/cyclic reachability for {}",
                inventory.target,
                canonical.display()
            ));
        }
        return Ok(());
    }
    let source = fs::read_to_string(&canonical).map_err(|error| {
        format!(
            "stage 1 {}: unreadable source {}: {error}",
            inventory.target,
            canonical.display()
        )
    })?;
    let file = syn::parse_file(&source).map_err(|error| {
        format!(
            "stage 1 {}: malformed source {}: {error}",
            inventory.target,
            canonical.display()
        )
    })?;
    if !visiting.insert(canonical.clone()) {
        return Err(format!(
            "stage 1 {}: cyclic module edge at {}",
            inventory.target,
            canonical.display()
        ));
    }
    inventory.active.insert(
        canonical.clone(),
        InventorySource {
            root: root.to_path_buf(),
            module_path: module_path.clone(),
            parent_edge,
            cfg_selection,
            source,
        },
    );
    for item in &file.items {
        if let Item::Macro(item) = item
            && (item.mac.path.is_ident("include") || item.mac.tokens.to_string().contains("mod"))
        {
            return Err(format!(
                "stage 1 {}: generated or macro source loading in {}",
                inventory.target,
                canonical.display()
            ));
        }
        let Item::Mod(module) = item else { continue };
        let selection = cfg_selection_for(&module.attrs, facts).map_err(|error| {
            format!(
                "stage 1 {}: {error} on module {} in {}",
                inventory.target,
                module.ident,
                canonical.display()
            )
        })?;
        let active = selection.selected;
        let name = module.ident.to_string();
        if module.content.is_some() {
            if !active {
                continue;
            }
            return Err(format!(
                "stage 1 {}: inline module {name} in {}",
                inventory.target,
                canonical.display()
            ));
        }
        if module
            .attrs
            .iter()
            .any(|attribute| attribute.path().is_ident("path"))
        {
            return Err(format!(
                "stage 1 {}: path redirected module {name} in {}",
                inventory.target,
                canonical.display()
            ));
        }
        let flat = child_directory.join(format!("{name}.rs"));
        let nested = child_directory.join(&name).join("mod.rs");
        let flat_exists = flat.is_file();
        let nested_exists = nested.is_file();
        if flat_exists == nested_exists {
            return Err(format!(
                "stage 1 {}: missing or ambiguous child {name} from {}",
                inventory.target,
                canonical.display()
            ));
        }
        let child = if flat_exists { flat } else { nested };
        if !active {
            let mut child_module = module_path.clone();
            child_module.push(name.clone());
            let mut inactive_walk = InactiveWalk {
                root,
                inventory,
                declared,
            };
            mark_inactive_source(
                InactiveSource {
                    source_path: child.clone(),
                    child_directory: module_directory(&child),
                    module_path: child_module,
                    parent_edge: Some(format!("{} -> {name}", canonical.display())),
                    cfg_selection: selection,
                },
                &mut inactive_walk,
            )?;
            continue;
        }
        declared.insert(child.clone());
        // Remove the declaration-only marker so the recursive visit owns duplicate detection.
        declared.remove(&child);
        let mut child_module = module_path.clone();
        child_module.push(name.clone());
        inventory_walk(
            &child,
            module_directory(&child),
            child_module,
            Some(format!("{} -> {name}", canonical.display())),
            root,
            selection,
            facts,
            inventory,
            declared,
            visiting,
        )?;
    }
    visiting.remove(&canonical);
    Ok(())
}

fn mark_inactive_source(source: InactiveSource, walk: &mut InactiveWalk<'_>) -> Result<(), String> {
    let InactiveSource {
        source_path,
        child_directory,
        module_path,
        parent_edge,
        cfg_selection,
    } = source;
    if !walk.declared.insert(source_path.clone()) {
        return Ok(());
    }
    let source = fs::read_to_string(&source_path).map_err(|error| {
        format!(
            "stage 1 {}: unreadable inactive source {}: {error}",
            walk.inventory.target,
            source_path.display()
        )
    })?;
    walk.inventory.inactive.insert(
        source_path.clone(),
        InventorySource {
            root: walk.root.to_path_buf(),
            module_path: module_path.clone(),
            parent_edge,
            cfg_selection,
            source: source.clone(),
        },
    );
    let file = syn::parse_file(&source).map_err(|error| {
        format!(
            "stage 1 {}: malformed inactive source {}: {error}",
            walk.inventory.target,
            source_path.display()
        )
    })?;
    for item in file.items {
        let Item::Mod(module) = item else { continue };
        if module.content.is_some() {
            continue;
        }
        if module
            .attrs
            .iter()
            .any(|attribute| attribute.path().is_ident("path"))
        {
            return Err(format!(
                "stage 1 {}: path redirected inactive module {}",
                walk.inventory.target, module.ident
            ));
        }
        let name = module.ident.to_string();
        let flat = child_directory.join(format!("{name}.rs"));
        let nested = child_directory.join(&name).join("mod.rs");
        if flat.is_file() == nested.is_file() {
            return Err(format!(
                "stage 1 {}: missing or ambiguous inactive child {name}",
                walk.inventory.target
            ));
        }
        let child = if flat.is_file() { flat } else { nested };
        let selection =
            cfg_selection_for(&module.attrs, &walk.inventory.facts)?.excluded_by_parent();
        let mut child_module = module_path.clone();
        child_module.push(name.clone());
        mark_inactive_source(
            InactiveSource {
                source_path: child.clone(),
                child_directory: module_directory(&child),
                module_path: child_module,
                parent_edge: Some(format!("{} -> {name}", source_path.display())),
                cfg_selection: selection,
            },
            walk,
        )?;
    }
    Ok(())
}

fn rust_sources(directory: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut sources = BTreeSet::new();
    fn visit(directory: &Path, sources: &mut BTreeSet<PathBuf>) -> Result<(), String> {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.is_dir() {
                visit(&path, sources)?;
            } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
                sources.insert(path);
            }
        }
        Ok(())
    }
    visit(directory, &mut sources)?;
    Ok(sources)
}

fn cfg_item_active(attributes: &[Attribute], facts: &CfgFacts) -> Result<bool, String> {
    let mut active = true;
    for attribute in attributes {
        if attribute.path().is_ident("cfg") {
            let Meta::List(list) = &attribute.meta else {
                return Err("unknown cfg grammar".to_owned());
            };
            let values = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                )
                .map_err(|_| "unknown cfg grammar".to_owned())?;
            if values.len() != 1 {
                return Err("unknown cfg grammar".to_owned());
            }
            active &= evaluate_cfg_meta(values.first().expect("one cfg predicate"), facts)?;
        } else if attribute.path().is_ident("cfg_attr") {
            let Meta::List(list) = &attribute.meta else {
                return Err("unsupported cfg_attr grammar".to_owned());
            };
            let nested = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                )
                .map_err(|_| "unsupported cfg_attr grammar".to_owned())?;
            let mut iter = nested.iter();
            let predicate = iter
                .next()
                .ok_or_else(|| "unsupported cfg_attr grammar".to_owned())?;
            if evaluate_cfg_meta(predicate, facts)? {
                for result in iter {
                    if !result.path().is_ident("cfg") {
                        return Err("unsupported cfg_attr result".to_owned());
                    }
                    active &= evaluate_cfg_attribute(result, facts)?;
                }
            }
        }
    }
    Ok(active)
}

impl CfgSelection {
    fn unconditional() -> Self {
        Self {
            predicates: vec![("unconditional".to_owned(), true)],
            selected: true,
            exclusion: None,
        }
    }

    fn excluded_by_parent(mut self) -> Self {
        self.selected = false;
        self.exclusion = Some("inactive parent module".to_owned());
        self
    }
}

/// Preserve the Stage-1 predicate decisions alongside the source manifest rather than reducing inactive sources to path names.
/// This is intentionally separate from the evaluator so all existing callers retain the same fail-closed cfg grammar.
fn cfg_selection_for(attributes: &[Attribute], facts: &CfgFacts) -> Result<CfgSelection, String> {
    let mut predicates = Vec::new();
    for attribute in attributes {
        if attribute.path().is_ident("cfg") {
            let Meta::List(list) = &attribute.meta else {
                return Err("unknown cfg grammar".to_owned());
            };
            let values = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                )
                .map_err(|_| "unknown cfg grammar".to_owned())?;
            if values.len() != 1 {
                return Err("unknown cfg grammar".to_owned());
            }
            let predicate = values.first().expect("one cfg predicate");
            predicates.push((
                cfg_meta_label(predicate),
                evaluate_cfg_meta(predicate, facts)?,
            ));
        } else if attribute.path().is_ident("cfg_attr") {
            let Meta::List(list) = &attribute.meta else {
                return Err("unsupported cfg_attr grammar".to_owned());
            };
            let nested = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                )
                .map_err(|_| "unsupported cfg_attr grammar".to_owned())?;
            let mut iter = nested.iter();
            let predicate = iter
                .next()
                .ok_or_else(|| "unsupported cfg_attr grammar".to_owned())?;
            let truth = evaluate_cfg_meta(predicate, facts)?;
            predicates.push((format!("cfg_attr({})", cfg_meta_label(predicate)), truth));
            if truth {
                for result in iter {
                    if !result.path().is_ident("cfg") {
                        return Err("unsupported cfg_attr result".to_owned());
                    }
                    predicates.push((
                        format!("cfg_attr result {}", cfg_meta_label(result)),
                        evaluate_cfg_attribute(result, facts)?,
                    ));
                }
            }
        }
    }
    let selected = cfg_item_active(attributes, facts)?;
    let exclusion = (!selected).then(|| "cfg predicate evaluated false".to_owned());
    if predicates.is_empty() {
        predicates.push(("unconditional".to_owned(), true));
    }
    Ok(CfgSelection {
        predicates,
        selected,
        exclusion,
    })
}

fn evaluate_cfg_attribute(meta: &Meta, facts: &CfgFacts) -> Result<bool, String> {
    let Meta::List(list) = meta else {
        return Err("unknown cfg grammar".to_owned());
    };
    let values = list
        .parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated)
        .map_err(|_| "unknown cfg grammar".to_owned())?;
    if values.len() != 1 {
        return Err("unknown cfg grammar".to_owned());
    }
    evaluate_cfg_meta(values.first().expect("one cfg predicate"), facts)
}

fn cfg_meta_label(meta: &Meta) -> String {
    match meta {
        Meta::Path(path) => path
            .get_ident()
            .map(ToString::to_string)
            .unwrap_or_else(|| "<unsupported-path>".to_owned()),
        Meta::NameValue(name_value) => match &name_value.value {
            Expr::Lit(literal) => match &literal.lit {
                syn::Lit::Str(value) => format!(
                    "{}={:?}",
                    name_value
                        .path
                        .get_ident()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    value.value()
                ),
                _ => "<unsupported-name-value>".to_owned(),
            },
            _ => "<unsupported-name-value>".to_owned(),
        },
        Meta::List(list) => {
            let values = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                )
                .map(|values| {
                    values
                        .iter()
                        .map(cfg_meta_label)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|_| "<unsupported>".to_owned());
            format!(
                "{}({values})",
                list.path
                    .get_ident()
                    .map(ToString::to_string)
                    .unwrap_or_default()
            )
        }
    }
}

fn evaluate_cfg_meta(meta: &Meta, facts: &CfgFacts) -> Result<bool, String> {
    match meta {
        Meta::Path(path) => {
            let name = path
                .get_ident()
                .ok_or_else(|| "unknown cfg grammar".to_owned())?
                .to_string();
            if !facts.knows_name(&name) {
                return Err("unknown cfg predicate".to_owned());
            }
            Ok(facts.has_bare(&name))
        }
        Meta::NameValue(value) => {
            let name = value
                .path
                .segments
                .last()
                .ok_or_else(|| "unknown cfg".to_owned())?
                .ident
                .to_string();
            let syn::Expr::Lit(literal) = &value.value else {
                return Err("unknown cfg grammar".to_owned());
            };
            let syn::Lit::Str(value) = &literal.lit else {
                return Err("unknown cfg grammar".to_owned());
            };
            facts.has_value(&name, &value.value())
        }
        Meta::List(list) => {
            let name = list
                .path
                .segments
                .last()
                .ok_or_else(|| "unknown cfg grammar".to_owned())?
                .ident
                .to_string();
            let values = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                )
                .map_err(|_| "unknown cfg grammar".to_owned())?;
            match name.as_str() {
                "all" => values
                    .iter()
                    .map(|value| evaluate_cfg_meta(value, facts))
                    .try_fold(true, |all, value| value.map(|value| all && value)),
                "any" => values
                    .iter()
                    .map(|value| evaluate_cfg_meta(value, facts))
                    .try_fold(false, |any, value| value.map(|value| any || value)),
                "not" if values.len() == 1 => Ok(!evaluate_cfg_meta(
                    values.first().expect("one cfg argument"),
                    facts,
                )?),
                _ => Err("unknown cfg grammar".to_owned()),
            }
        }
    }
}

fn assert_executor_inventory(inventory: &SourceInventory, target: &str) {
    let unix = inventory
        .active
        .keys()
        .any(|path| path.ends_with("src/executor/directory_link.rs"));
    let windows = inventory
        .active
        .keys()
        .any(|path| path.ends_with("src/executor/directory_link_windows.rs"));
    match target {
        "x86_64-unknown-linux-gnu" | "aarch64-apple-darwin" => {
            assert!(unix && !windows, "{target}")
        }
        "x86_64-pc-windows-msvc" => assert!(!unix && windows, "{target}"),
        _ => unreachable!(),
    }
}

/// Emit the complete finite Stage-1 manifest so `--nocapture` evidence is independently auditable.
/// The structural scan consumes the same records; reporting never re-walks `src`.
fn report_stage_one_inventory(inventory: &SourceInventory) {
    for root in &inventory.roots {
        println!("stage1 target={} root={}", inventory.target, root.display());
    }
    for (path, source) in inventory.active.iter().chain(inventory.inactive.iter()) {
        let predicates = source
            .cfg_selection
            .predicates
            .iter()
            .map(|(predicate, truth)| format!("{predicate}:{truth}"))
            .collect::<Vec<_>>()
            .join("; ");
        let module_path = if source.module_path.is_empty() {
            "crate".to_owned()
        } else {
            format!("crate::{}", source.module_path.join("::"))
        };
        println!(
            "stage1 target={} root={} source={} module_path={} parent_edge={} cfg_predicate_truth={} selected={} exclusion={}",
            inventory.target,
            source.root.display(),
            path.display(),
            module_path,
            source.parent_edge.as_deref().unwrap_or("<crate-root>"),
            predicates,
            source.cfg_selection.selected,
            source.cfg_selection.exclusion.as_deref().unwrap_or("none"),
        );
    }
}

struct InventoryFixture {
    root: PathBuf,
}

impl InventoryFixture {
    fn new() -> Self {
        let nonce = NEXT_INVENTORY_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "loadout-migration-inventory-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create unique inventory fixture");
        Self { root }
    }
}

impl Drop for InventoryFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn active_external_module_edges(file: &File, facts: &CfgFacts) -> Vec<(String, PathBuf)> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Mod(module)
                if module.content.is_none() && item_is_active(&module.attrs, facts) =>
            {
                let name = module.ident.to_string();
                (name.starts_with("directory_link"))
                    .then(|| (name.clone(), PathBuf::from(format!("{name}.rs"))))
            }
            _ => None,
        })
        .collect()
}

fn active_directory_link_reexports(file: &File, facts: &CfgFacts) -> Vec<(String, String)> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Use(item) if item_is_active(&item.attrs, facts) => use_rename(&item.tree),
            _ => None,
        })
        .filter(|(from, to)| from == "directory_link_windows" || to == "directory_link")
        .collect()
}

fn validate_windows_executor_inventory(file: &File, facts: &CfgFacts) -> Result<(), String> {
    if file.items.iter().any(|item| matches!(item, Item::Mod(module) if module.attrs.iter().any(|attribute| attribute.path().is_ident("path")))) {
        return Err("path-redirected Windows declaration rule".to_owned());
    }
    let edges = active_external_module_edges(file, facts);
    let reexports = active_directory_link_reexports(file, facts);
    if edges
        != [(
            "directory_link_windows".to_owned(),
            PathBuf::from("directory_link_windows.rs"),
        )]
    {
        return Err("Windows declaration rule".to_owned());
    }
    if reexports
        != [(
            "directory_link_windows".to_owned(),
            "directory_link".to_owned(),
        )]
    {
        return Err("Windows re-export rule".to_owned());
    }
    Ok(())
}

fn validate_unix_executor_inventory(file: &File, facts: &CfgFacts) -> Result<(), String> {
    if file.items.iter().any(|item| matches!(item, Item::Mod(module) if module.content.is_some() || module.attrs.iter().any(|attribute| attribute.path().is_ident("path")))) {
        return Err("Unix declaration must be a conventional external module".to_owned());
    }
    let edges = active_external_module_edges(file, facts);
    if edges
        != [(
            "directory_link".to_owned(),
            PathBuf::from("directory_link.rs"),
        )]
    {
        return Err("Unix declaration rule".to_owned());
    }
    if !active_directory_link_reexports(file, facts).is_empty() {
        return Err("Unix re-export rule".to_owned());
    }
    Ok(())
}

fn use_rename(tree: &UseTree) -> Option<(String, String)> {
    match tree {
        UseTree::Rename(rename) => Some((rename.ident.to_string(), rename.rename.to_string())),
        UseTree::Path(path) => use_rename(&path.tree),
        _ => None,
    }
}

fn item_is_active(attributes: &[syn::Attribute], facts: &CfgFacts) -> bool {
    cfg_item_active(attributes, facts).expect("literal cfg predicate")
}

#[test]
fn production_tree_exposes_only_a_private_facade_edge() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    for record in exact_target_records() {
        let inventory = build_source_inventory(&record).expect("stage-one production inventory");
        violations.extend(scan_inventory_sources(&inventory));
    }
    assert_private_module(&root.join("lib.rs"), "migration", &mut violations);
    assert_private_module(
        &root.join("migration/mod.rs"),
        "schema_3_to_4",
        &mut violations,
    );

    assert!(
        violations.is_empty(),
        "migration isolation violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn oracle_accepts_the_direct_data_only_facade_call() {
    let source = r#"
        use crate::migration::{MigrationFacade, MigrationOutcome, MigrationPresentation, MigrationRequest};
        fn dispatch(request: MigrationRequest) -> Result<MigrationOutcome, ()> {
            let _presentation: MigrationPresentation = MigrationFacade::present(&request);
            MigrationFacade::execute(request).map_err(|_| ())
        }
    "#;
    assert_eq!(fixture_rule(source), None);
}

#[test]
fn oracle_rejects_every_required_facade_bypass_class() {
    for (name, source, expected) in [
        (
            "route-private-import",
            "use crate::migration::schema_3_to_4::convert_state; fn f() {}",
            "route-private migration import",
        ),
        (
            "legacy-parser-alias",
            "use serde_yaml::from_str as parse; fn f() { let _ = parse::<String>(\"x\"); }",
            "legacy parser or converter",
        ),
        (
            "legacy-parser-glob",
            "use serde_yaml::*; fn f() { let _ = from_str::<String>(\"x\"); }",
            "legacy parser or converter",
        ),
        (
            "legacy-json-parser-qualified",
            "fn f() { let _ = serde_json::from_slice::<String>(b\"x\"); }",
            "legacy parser or converter",
        ),
        (
            "legacy-converter-alias",
            "use crate::migration::schema_3_to_4::convert_profile as convert; fn f() { let _ = convert(\"x\"); }",
            "route-private migration import",
        ),
        (
            "state-lock-alias",
            "use crate::state::repository::StateRepository as Repository; fn f() { let _ = Repository::acquire_exclusive_for_migration; }",
            "state lock before facade",
        ),
        (
            "publication-function-alias",
            "use std::fs::rename as publish; fn f() { let _ = publish(\"a\", \"b\"); }",
            "publication capability before facade",
        ),
        (
            "publication-method",
            "fn f(writer: std::fs::File) { let _ = writer.sync_all(); }",
            "publication capability before facade",
        ),
        (
            "inventory-filesystem-read",
            "fn f() { let _ = std::fs::read_dir(\"profiles\"); }",
            "publication capability before facade",
        ),
        (
            "indirect-facade-dispatch",
            "fn f() { let call = MigrationFacade::execute; let _ = call; }",
            "indirect facade dispatch",
        ),
        (
            "indirect-facade-dispatch-through-alias",
            "use crate::migration::MigrationFacade as Facade; fn f() { let call = Facade::execute; call; }",
            "indirect facade dispatch",
        ),
        (
            "zero-facade-dispatch",
            "fn dispatch() {}",
            "zero facade dispatch",
        ),
        (
            "multiple-facade-dispatch",
            "fn dispatch(request: MigrationRequest) { MigrationFacade::execute(request.clone()); MigrationFacade::execute(request); }",
            "multiple facade dispatches",
        ),
        (
            "wrapper-facade-dispatch",
            "fn facade_wrapper(request: MigrationRequest) { MigrationFacade::execute(request); } fn dispatch(request: MigrationRequest) { facade_wrapper(request); }",
            "indirect facade dispatch",
        ),
        (
            "macro-facade-dispatch",
            "fn dispatch() { invoke!(MigrationFacade::execute); }",
            "generated or macro facade dispatch",
        ),
    ] {
        assert_eq!(fixture_rule(source), Some(expected), "{name}");
    }
}

#[test]
fn production_adapter_analysis_enforces_capabilities_and_counts_real_dispatches() {
    for (source, expected) in [
        (
            "use crate::migration::MigrationFacade; use serde_yaml::from_str; fn dispatch() { let _ = from_str::<String>(\"x\"); let _ = MigrationFacade::execute; }",
            "legacy parser or converter",
        ),
        (
            "use crate::migration::MigrationFacade; use crate::state::repository::StateRepository; fn dispatch() { let _ = StateRepository::acquire_exclusive_for_migration; let _ = MigrationFacade::execute; }",
            "state lock before facade",
        ),
        (
            "use crate::migration::MigrationFacade; use std::fs::rename; fn dispatch() { let _ = rename(\"a\", \"b\"); let _ = MigrationFacade::execute; }",
            "publication capability before facade",
        ),
        (
            "use crate::migration::{MigrationFacade, MigrationRequest}; fn dispatch(request: MigrationRequest) { MigrationFacade::execute(request.clone()); MigrationFacade::execute(request); }",
            "multiple facade dispatches",
        ),
        (
            "use crate::migration::MigrationFacade; fn dispatch(writer: std::fs::File) { let _ = writer.sync_all(); MigrationFacade::execute; }",
            "publication capability before facade",
        ),
        (
            "fn dispatch() { invoke!(crate::migration::MigrationFacade::execute); }",
            "generated or macro facade dispatch",
        ),
        (
            "use crate::migration::MigrationFacade; fn dispatch() {}",
            "zero facade dispatch",
        ),
    ] {
        assert_eq!(production_rule(source), Some(expected));
    }
}

#[test]
fn production_oracle_rejects_a_cross_module_facade_wrapper() {
    let source = "use crate::migration::{MigrationFacade, MigrationRequest}; fn execute(request: MigrationRequest) { MigrationFacade::execute(request); }";
    assert_eq!(
        production_rule_at(source, "authoring/migrate.rs"),
        Some("migration facade dispatch outside terminal adapter")
    );
}

fn scan_inventory_sources(inventory: &SourceInventory) -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    for (path, source_record) in &inventory.active {
        let _recorded_edge = (&source_record.module_path, &source_record.parent_edge);
        if path.starts_with(root.join("migration")) {
            continue;
        }
        let file = match syn::parse_file(&source_record.source) {
            Ok(file) => file,
            Err(error) => {
                violations.push(format!(
                    "{}: Rust parse failure: {error}",
                    display(&root, path)
                ));
                continue;
            }
        };
        let active_reference =
            match MigrationReferenceDetector::contains_active(&file, &inventory.facts) {
                Ok(active_reference) => active_reference,
                Err(error) => {
                    violations.push(format!(
                        "{}: cfg selection failed: {error}",
                        display(&root, path)
                    ));
                    continue;
                }
            };
        if active_reference && display(&root, path) != "cli/mod.rs" {
            violations.push(format!(
                "{}: migration facade dispatch outside terminal adapter",
                display(&root, path)
            ));
        }
        let mut oracle = Oracle::production();
        // Current-schema readers elsewhere in the crate legitimately parse documents and use
        // filesystem capabilities. A production file becomes an adapter candidate only through
        // a syntactically resolved migration reference; once it does, every prohibited
        // capability is checked exactly as it is in a negative fixture.
        oracle.enforce_capabilities = active_reference;
        oracle.cfg_facts = Some(inventory.facts.clone());
        oracle.visit_file(&file);
        oracle.finish_production();
        violations.extend(
            oracle
                .violations
                .into_iter()
                .map(|rule| format!("{}: {rule}", display(&root, path))),
        );
    }
    violations
}

/// Stage two accepts a stage-one manifest and scans only items selected for that record.
/// An inactive parser, legacy DTO, or dispatch item therefore cannot be used as evidence for
/// either the passing or failing configuration.
fn active_items_only(mut file: File, facts: &CfgFacts) -> Result<File, String> {
    let mut active = Vec::with_capacity(file.items.len());
    for item in file.items {
        let selected = match item_attributes(&item) {
            Some(attributes) => cfg_item_active(attributes, facts)?,
            None => true,
        };
        if selected {
            active.push(item);
        }
    }
    file.items = active;
    Ok(file)
}

#[test]
fn stage_two_cfg_and_cfg_attr_errors_are_reported_not_masked_as_inactive() {
    let facts = CfgFacts::from_pairs([("unix".to_owned(), None)]);
    for source in [
        "#[cfg(unmodelled)] fn rejected() {}",
        "#[cfg_attr(unix, cfg(feature = \"unmodelled\"))] fn rejected() {}",
    ] {
        let file = syn::parse_file(source).expect("cfg fixture parses");
        assert!(
            active_items_only(file, &facts).is_err(),
            "Stage 2 must fail closed for {source}"
        );
    }
}

#[test]
fn stage_two_evaluates_nested_cfg_and_cfg_attr_before_structural_scanning() {
    let unix = CfgFacts::from_pairs([("unix".to_owned(), None)]);
    let inactive =
        syn::parse_file("mod nested { #[cfg(windows)] use crate::migration::MigrationFacade; }")
            .expect("nested cfg fixture parses");
    assert!(
        !MigrationReferenceDetector::contains_active(&inactive, &unix)
            .expect("inactive nested cfg must be evaluated"),
        "an inactive nested item must not be structurally scanned"
    );
    for source in [
        "mod nested { #[cfg(unmodelled)] use crate::migration::MigrationFacade; }",
        "mod nested { #[cfg_attr(unix, cfg(feature = \"unmodelled\"))] use crate::migration::MigrationFacade; }",
    ] {
        let file = syn::parse_file(source).expect("nested cfg fixture parses");
        assert!(
            MigrationReferenceDetector::contains_active(&file, &unix).is_err(),
            "nested cfg/cfg_attr errors must fail closed before structural scanning: {source}"
        );
        let mut oracle = Oracle::production();
        oracle.cfg_facts = Some(unix.clone());
        oracle.visit_file(&file);
        assert!(
            oracle.violations.contains(&"cfg selection failed"),
            "nested cfg/cfg_attr errors must be enforced by the stage-two oracle: {source}"
        );
    }
}

#[test]
fn stage_two_evaluates_cfg_on_impl_and_trait_members_before_structural_scanning() {
    let unix = CfgFacts::from_pairs([("unix".to_owned(), None)]);
    for source in [
        "struct Adapter; impl Adapter { #[cfg(windows)] fn run() { let _ = crate::migration::MigrationFacade::execute; } }",
        "trait Adapter { #[cfg(windows)] fn run() { let _ = crate::migration::MigrationFacade::execute; } }",
    ] {
        let file = syn::parse_file(source).expect("nested associated-item fixture parses");
        assert!(
            !MigrationReferenceDetector::contains_active(&file, &unix)
                .expect("inactive associated item must be evaluated"),
            "an inactive associated item must not be structurally scanned: {source}"
        );
    }
    for source in [
        "struct Adapter; impl Adapter { #[cfg_attr(unix, cfg(feature = \"unmodelled\"))] fn run() { let _ = crate::migration::MigrationFacade::execute; } }",
        "trait Adapter { #[cfg_attr(unix, cfg(feature = \"unmodelled\"))] fn run() { let _ = crate::migration::MigrationFacade::execute; } }",
    ] {
        let file = syn::parse_file(source).expect("associated cfg_attr fixture parses");
        assert!(
            MigrationReferenceDetector::contains_active(&file, &unix).is_err(),
            "associated cfg/cfg_attr errors must fail closed: {source}"
        );
        let mut oracle = Oracle::production();
        oracle.cfg_facts = Some(unix.clone());
        oracle.visit_file(&file);
        assert!(oracle.violations.contains(&"cfg selection failed"));
    }
}

fn item_attributes(item: &Item) -> Option<&[Attribute]> {
    match item {
        Item::Const(item) => Some(&item.attrs),
        Item::Enum(item) => Some(&item.attrs),
        Item::ExternCrate(item) => Some(&item.attrs),
        Item::Fn(item) => Some(&item.attrs),
        Item::ForeignMod(item) => Some(&item.attrs),
        Item::Impl(item) => Some(&item.attrs),
        Item::Macro(item) => Some(&item.attrs),
        Item::Mod(item) => Some(&item.attrs),
        Item::Static(item) => Some(&item.attrs),
        Item::Struct(item) => Some(&item.attrs),
        Item::Trait(item) => Some(&item.attrs),
        Item::TraitAlias(item) => Some(&item.attrs),
        Item::Type(item) => Some(&item.attrs),
        Item::Union(item) => Some(&item.attrs),
        Item::Use(item) => Some(&item.attrs),
        _ => None,
    }
}

fn impl_item_attributes(item: &ImplItem) -> Option<&[Attribute]> {
    match item {
        ImplItem::Const(item) => Some(&item.attrs),
        ImplItem::Fn(item) => Some(&item.attrs),
        ImplItem::Macro(item) => Some(&item.attrs),
        ImplItem::Type(item) => Some(&item.attrs),
        ImplItem::Verbatim(_) => None,
        _ => None,
    }
}

fn trait_item_attributes(item: &TraitItem) -> Option<&[Attribute]> {
    match item {
        TraitItem::Const(item) => Some(&item.attrs),
        TraitItem::Fn(item) => Some(&item.attrs),
        TraitItem::Macro(item) => Some(&item.attrs),
        TraitItem::Type(item) => Some(&item.attrs),
        TraitItem::Verbatim(_) => None,
        _ => None,
    }
}

fn assert_private_module(path: &Path, expected_name: &str, violations: &mut Vec<String>) {
    let source = fs::read_to_string(path).expect("migration module source must be readable");
    let file: File = syn::parse_file(&source).expect("migration module source must parse");
    let module = file.items.iter().find_map(|item| match item {
        Item::Mod(module) if module.ident == expected_name => Some(module),
        _ => None,
    });
    match module {
        Some(ItemMod {
            vis: syn::Visibility::Inherited,
            ..
        }) => {}
        _ => violations.push(format!(
            "{}: {expected_name} must remain a private module",
            path.display()
        )),
    }
}

fn fixture_rule(source: &str) -> Option<&'static str> {
    let file = syn::parse_file(source).expect("oracle fixture must parse");
    let mut oracle = Oracle::fixture();
    oracle.visit_file(&file);
    oracle.finish_fixture();
    oracle.violations.into_iter().next()
}

fn production_rule(source: &str) -> Option<&'static str> {
    let file = syn::parse_file(source).expect("production oracle fixture must parse");
    let mut oracle = Oracle::production();
    oracle.enforce_capabilities = MigrationReferenceDetector::contains(&file);
    oracle.visit_file(&file);
    oracle.finish_production();
    oracle.violations.into_iter().next()
}

fn production_rule_at(source: &str, relative_path: &str) -> Option<&'static str> {
    let file = syn::parse_file(source).expect("production oracle fixture must parse");
    if MigrationReferenceDetector::contains(&file) && relative_path != "cli/mod.rs" {
        return Some("migration facade dispatch outside terminal adapter");
    }
    let mut oracle = Oracle::production();
    oracle.enforce_capabilities = MigrationReferenceDetector::contains(&file);
    oracle.visit_file(&file);
    oracle.finish_production();
    oracle.violations.into_iter().next()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Binding {
    Facade,
    FacadeDispatch,
    RoutePrivateMigration,
    LegacyParserOrConverter,
    StateLock,
    Publication,
}

struct Oracle {
    fixture: bool,
    enforce_capabilities: bool,
    scopes: Vec<BTreeMap<String, Binding>>,
    violations: Vec<&'static str>,
    facade_dispatches: usize,
    current_functions: Vec<String>,
    facade_functions: Vec<String>,
    local_calls: Vec<(String, String)>,
    cfg_facts: Option<CfgFacts>,
}

impl Oracle {
    fn production() -> Self {
        Self {
            fixture: false,
            enforce_capabilities: false,
            scopes: vec![BTreeMap::new()],
            violations: Vec::new(),
            facade_dispatches: 0,
            current_functions: Vec::new(),
            facade_functions: Vec::new(),
            local_calls: Vec::new(),
            cfg_facts: None,
        }
    }

    fn fixture() -> Self {
        Self {
            fixture: true,
            enforce_capabilities: true,
            scopes: vec![BTreeMap::new()],
            violations: Vec::new(),
            facade_dispatches: 0,
            current_functions: Vec::new(),
            facade_functions: Vec::new(),
            local_calls: Vec::new(),
            cfg_facts: None,
        }
    }

    fn reject(&mut self, rule: &'static str) {
        if !self.violations.contains(&rule) {
            self.violations.push(rule);
        }
    }

    fn cfg_active(&mut self, attributes: &[Attribute]) -> bool {
        match &self.cfg_facts {
            Some(facts) => match cfg_item_active(attributes, facts) {
                Ok(active) => active,
                Err(_) => {
                    self.reject("cfg selection failed");
                    false
                }
            },
            None => true,
        }
    }

    fn finish_fixture(&mut self) {
        debug_assert!(self.fixture);
        self.finish_dispatches(true);
    }

    fn finish_production(&mut self) {
        // Production source location is checked by `scan_production_tree`. Within the sole
        // terminal adapter, calling its command handler is normal command dispatch, not a
        // facade wrapper. The one direct facade execution is still counted here.
        self.finish_dispatches(true);
    }

    fn finish_dispatches(&mut self, require_dispatch: bool) {
        if !self.enforce_capabilities {
            return;
        }
        match self.facade_dispatches {
            0 if require_dispatch => self.reject("zero facade dispatch"),
            1 => {}
            0 => {}
            _ => self.reject("multiple facade dispatches"),
        }
        if self.fixture
            && self.local_calls.iter().any(|(_, callee)| {
                self.facade_functions
                    .iter()
                    .any(|facade_function| facade_function == callee)
            })
        {
            self.reject("indirect facade dispatch");
        }
    }

    fn bind(&mut self, name: String, binding: Binding) {
        self.scopes
            .last_mut()
            .expect("oracle always has a scope")
            .insert(name, binding);
    }

    fn resolve(&self, name: &str) -> Option<Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn classify_path(&self, path: &SynPath) -> Option<Binding> {
        let segments = path_segments(path);
        if segments == ["MigrationFacade"] {
            return self.resolve("MigrationFacade").or(Some(Binding::Facade));
        }
        if segments.len() == 1 {
            return self.resolve(&segments[0]);
        }
        if segments.last().is_some_and(|segment| segment == "execute")
            && (segments[segments.len() - 2] == "MigrationFacade"
                || self.resolve(&segments[segments.len() - 2]) == Some(Binding::Facade))
        {
            return Some(Binding::FacadeDispatch);
        }
        if ends_with(&segments, &["migration", "schema_3_to_4"])
            || segments.iter().any(|segment| segment == "schema_3_to_4")
        {
            return Some(Binding::RoutePrivateMigration);
        }
        if ends_with(&segments, &["serde_yaml", "from_str"])
            || ends_with(&segments, &["serde_json", "from_str"])
            || ends_with(&segments, &["serde_json", "from_slice"])
            || ends_with(&segments, &["serde_json", "from_reader"])
        {
            return Some(Binding::LegacyParserOrConverter);
        }
        if ends_with(
            &segments,
            &["StateRepository", "acquire_exclusive_for_migration"],
        ) || (segments
            .last()
            .is_some_and(|segment| segment == "acquire_exclusive_for_migration")
            && self.resolve(&segments[segments.len() - 2]) == Some(Binding::StateLock))
        {
            return Some(Binding::StateLock);
        }
        if ends_with(&segments, &["fs", "rename"])
            || ends_with(&segments, &["fs", "read"])
            || ends_with(&segments, &["fs", "read_to_string"])
            || ends_with(&segments, &["fs", "read_dir"])
            || ends_with(&segments, &["fs", "metadata"])
            || ends_with(&segments, &["fs", "symlink_metadata"])
            || ends_with(&segments, &["File", "open"])
            || segments
                .last()
                .is_some_and(|segment| segment == "OpenOptions")
            || segments
                .last()
                .is_some_and(|segment| segment == "publish_current")
        {
            return Some(Binding::Publication);
        }
        None
    }

    fn check_binding(&mut self, binding: Binding) {
        match binding {
            Binding::Facade | Binding::FacadeDispatch => {}
            Binding::LegacyParserOrConverter if self.enforce_capabilities => {
                self.reject("legacy parser or converter")
            }
            Binding::RoutePrivateMigration => self.reject("route-private migration import"),
            Binding::StateLock if self.enforce_capabilities => {
                self.reject("state lock before facade")
            }
            Binding::Publication if self.enforce_capabilities => {
                self.reject("publication capability before facade")
            }
            _ => {}
        }
    }

    fn bind_use_tree(&mut self, prefix: &mut Vec<String>, tree: &UseTree) {
        match tree {
            UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.bind_use_tree(prefix, &path.tree);
                prefix.pop();
            }
            UseTree::Name(name) => {
                prefix.push(name.ident.to_string());
                self.bind_import(prefix, name.ident.to_string());
                prefix.pop();
            }
            UseTree::Rename(rename) => {
                prefix.push(rename.ident.to_string());
                self.bind_import(prefix, rename.rename.to_string());
                prefix.pop();
            }
            UseTree::Group(group) => {
                for item in &group.items {
                    self.bind_use_tree(prefix, item);
                }
            }
            UseTree::Glob(_) if prefix.iter().any(|segment| segment == "migration") => {
                self.reject("non-facade migration import");
            }
            UseTree::Glob(_) if ends_with(prefix, &["serde_yaml"]) => {
                self.bind("from_str".to_owned(), Binding::LegacyParserOrConverter);
            }
            UseTree::Glob(_) if ends_with(prefix, &["serde_json"]) => {
                for name in ["from_str", "from_slice", "from_reader"] {
                    self.bind(name.to_owned(), Binding::LegacyParserOrConverter);
                }
            }
            UseTree::Glob(_) => {}
        }
    }

    fn bind_import(&mut self, path: &[String], local_name: String) {
        if let Some(index) = path.iter().position(|segment| segment == "migration") {
            let suffix = &path[index + 1..];
            if suffix.len() != 1
                || !matches!(
                    suffix[0].as_str(),
                    "MigrationFacade"
                        | "MigrationRequest"
                        | "MigrationPresentation"
                        | "MigrationOutcome"
                        | "MigrationFacadeError"
                        | "MigrationOperation"
                )
            {
                self.reject(
                    if suffix
                        .first()
                        .is_some_and(|segment| segment == "schema_3_to_4")
                    {
                        "route-private migration import"
                    } else {
                        "non-facade migration import"
                    },
                );
            }
            if suffix == ["MigrationFacade"] {
                self.bind(local_name, Binding::Facade);
            }
            return;
        }
        if let Some(binding) = self.classify_import_path(path) {
            self.bind(local_name, binding);
        }
    }

    fn classify_import_path(&self, path: &[String]) -> Option<Binding> {
        if ends_with(path, &["serde_yaml", "from_str"])
            || ends_with(path, &["serde_json", "from_str"])
            || ends_with(path, &["serde_json", "from_slice"])
            || ends_with(path, &["serde_json", "from_reader"])
        {
            return Some(Binding::LegacyParserOrConverter);
        }
        if ends_with(path, &["fs", "rename"])
            || ends_with(path, &["fs", "read"])
            || ends_with(path, &["fs", "read_to_string"])
            || ends_with(path, &["fs", "read_dir"])
            || ends_with(path, &["fs", "metadata"])
            || ends_with(path, &["fs", "symlink_metadata"])
            || ends_with(path, &["File", "open"])
            || path.last().is_some_and(|segment| segment == "OpenOptions")
        {
            return Some(Binding::Publication);
        }
        if path
            .last()
            .is_some_and(|segment| segment == "StateRepository")
        {
            return Some(Binding::StateLock);
        }
        None
    }
}

impl<'ast> Visit<'ast> for Oracle {
    fn visit_item(&mut self, item: &'ast Item) {
        if item_attributes(item).is_none_or(|attributes| self.cfg_active(attributes)) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        self.bind_use_tree(&mut Vec::new(), &item.tree);
        visit::visit_item_use(self, item);
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.scopes.push(BTreeMap::new());
        visit::visit_block(self, block);
        self.scopes.pop();
    }

    fn visit_local(&mut self, local: &'ast Local) {
        if !self.cfg_active(&local.attrs) {
            return;
        }
        visit::visit_local(self, local);
        let Some(name) = local_name(&local.pat) else {
            return;
        };
        let Some(init) = &local.init else { return };
        if let Some(binding) = expression_binding(&init.expr, self) {
            if binding == Binding::FacadeDispatch {
                self.reject("indirect facade dispatch");
            }
            self.bind(name, binding);
        }
    }

    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        self.current_functions.push(function.sig.ident.to_string());
        visit::visit_item_fn(self, function);
        self.current_functions.pop();
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        if impl_item_attributes(item).is_none_or(|attributes| self.cfg_active(attributes)) {
            visit::visit_impl_item(self, item);
        }
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        if trait_item_attributes(item).is_none_or(|attributes| self.cfg_active(attributes)) {
            visit::visit_trait_item(self, item);
        }
    }

    fn visit_expr_call(&mut self, expression: &'ast syn::ExprCall) {
        if let Expr::Path(path) = expression.func.as_ref() {
            match self.classify_path(&path.path) {
                Some(Binding::FacadeDispatch) => {
                    self.facade_dispatches += 1;
                    if let Some(function) = self.current_functions.last() {
                        self.facade_functions.push(function.clone());
                    }
                }
                _ if path.path.segments.len() == 1 => {
                    if let Some(function) = self.current_functions.last() {
                        self.local_calls
                            .push((function.clone(), path.path.segments[0].ident.to_string()));
                    }
                }
                _ => {}
            }
        }
        visit::visit_expr_call(self, expression);
    }

    fn visit_expr(&mut self, expression: &'ast Expr) {
        if self.cfg_active(expr_attributes(expression)) {
            visit::visit_expr(self, expression);
        }
    }

    fn visit_expr_macro(&mut self, expression: &'ast syn::ExprMacro) {
        if self.enforce_capabilities && macro_mentions_migration(&expression.mac) {
            // `syn` intentionally leaves arbitrary macro input unexpanded. A migration adapter
            // therefore cannot prove a generated call is the one direct facade call.
            self.reject("generated or macro facade dispatch");
        }
        visit::visit_expr_macro(self, expression);
    }

    fn visit_stmt_macro(&mut self, statement: &'ast StmtMacro) {
        if self.enforce_capabilities && macro_mentions_migration(&statement.mac) {
            self.reject("generated or macro facade dispatch");
        }
        visit::visit_stmt_macro(self, statement);
    }

    fn visit_expr_path(&mut self, expression: &'ast ExprPath) {
        if let Some(binding) = self.classify_path(&expression.path) {
            self.check_binding(binding);
        }
        visit::visit_expr_path(self, expression);
    }

    fn visit_expr_method_call(&mut self, expression: &'ast syn::ExprMethodCall) {
        if self.enforce_capabilities
            && matches!(
                expression.method.to_string().as_str(),
                "sync_all" | "publish_current"
            )
        {
            self.reject("publication capability before facade");
        }
        visit::visit_expr_method_call(self, expression);
    }
}

struct MigrationReferenceDetector {
    found: bool,
}

impl MigrationReferenceDetector {
    fn contains(file: &File) -> bool {
        let mut detector = Self { found: false };
        detector.visit_file(file);
        detector.found
    }

    fn contains_active(file: &File, facts: &CfgFacts) -> Result<bool, String> {
        let mut detector = ActiveMigrationReferenceDetector {
            found: false,
            facts,
            error: None,
        };
        detector.visit_file(file);
        detector.error.map_or(Ok(detector.found), Err)
    }
}

struct ActiveMigrationReferenceDetector<'a> {
    found: bool,
    facts: &'a CfgFacts,
    error: Option<String>,
}

impl ActiveMigrationReferenceDetector<'_> {
    fn active(&mut self, attributes: &[Attribute]) -> bool {
        match cfg_item_active(attributes, self.facts) {
            Ok(active) => active,
            Err(error) => {
                self.error.get_or_insert(error);
                false
            }
        }
    }
}

impl<'ast> Visit<'ast> for ActiveMigrationReferenceDetector<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        if item_attributes(item).is_none_or(|attributes| self.active(attributes)) {
            visit::visit_item(self, item);
        }
    }

    fn visit_local(&mut self, local: &'ast Local) {
        if self.active(&local.attrs) {
            visit::visit_local(self, local);
        }
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        if impl_item_attributes(item).is_none_or(|attributes| self.active(attributes)) {
            visit::visit_impl_item(self, item);
        }
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        if trait_item_attributes(item).is_none_or(|attributes| self.active(attributes)) {
            visit::visit_trait_item(self, item);
        }
    }

    fn visit_expr(&mut self, expression: &'ast Expr) {
        if self.active(expr_attributes(expression)) {
            visit::visit_expr(self, expression);
        }
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        self.found |= use_tree_contains_migration(&item.tree);
        visit::visit_item_use(self, item);
    }

    fn visit_path(&mut self, path: &'ast SynPath) {
        self.found |= path
            .segments
            .iter()
            .any(|segment| segment.ident == "migration");
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        self.found |= macro_mentions_migration(invocation);
        visit::visit_macro(self, invocation);
    }
}

fn expr_attributes(expression: &Expr) -> &[Attribute] {
    match expression {
        Expr::Array(value) => &value.attrs,
        Expr::Assign(value) => &value.attrs,
        Expr::Async(value) => &value.attrs,
        Expr::Await(value) => &value.attrs,
        Expr::Binary(value) => &value.attrs,
        Expr::Block(value) => &value.attrs,
        Expr::Break(value) => &value.attrs,
        Expr::Call(value) => &value.attrs,
        Expr::Cast(value) => &value.attrs,
        Expr::Closure(value) => &value.attrs,
        Expr::Const(value) => &value.attrs,
        Expr::Continue(value) => &value.attrs,
        Expr::Field(value) => &value.attrs,
        Expr::ForLoop(value) => &value.attrs,
        Expr::Group(value) => &value.attrs,
        Expr::If(value) => &value.attrs,
        Expr::Index(value) => &value.attrs,
        Expr::Infer(value) => &value.attrs,
        Expr::Let(value) => &value.attrs,
        Expr::Lit(value) => &value.attrs,
        Expr::Loop(value) => &value.attrs,
        Expr::Macro(value) => &value.attrs,
        Expr::Match(value) => &value.attrs,
        Expr::MethodCall(value) => &value.attrs,
        Expr::Paren(value) => &value.attrs,
        Expr::Path(value) => &value.attrs,
        Expr::Range(value) => &value.attrs,
        Expr::RawAddr(value) => &value.attrs,
        Expr::Reference(value) => &value.attrs,
        Expr::Repeat(value) => &value.attrs,
        Expr::Return(value) => &value.attrs,
        Expr::Struct(value) => &value.attrs,
        Expr::Try(value) => &value.attrs,
        Expr::TryBlock(value) => &value.attrs,
        Expr::Tuple(value) => &value.attrs,
        Expr::Unary(value) => &value.attrs,
        Expr::Unsafe(value) => &value.attrs,
        Expr::Verbatim(_) => &[],
        Expr::While(value) => &value.attrs,
        Expr::Yield(value) => &value.attrs,
        _ => &[],
    }
}

impl<'ast> Visit<'ast> for MigrationReferenceDetector {
    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        self.found |= use_tree_contains_migration(&item.tree);
        visit::visit_item_use(self, item);
    }

    fn visit_path(&mut self, path: &'ast SynPath) {
        self.found |= path
            .segments
            .iter()
            .any(|segment| segment.ident == "migration");
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        self.found |= macro_mentions_migration(invocation);
        visit::visit_macro(self, invocation);
    }
}

fn macro_mentions_migration(invocation: &syn::Macro) -> bool {
    invocation
        .path
        .segments
        .iter()
        .any(|segment| is_migration_marker(&segment.ident.to_string()))
        || token_trees_mention_migration(invocation.tokens.clone())
}

fn token_trees_mention_migration(tokens: proc_macro2::TokenStream) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Ident(ident) => is_migration_marker(&ident.to_string()),
        TokenTree::Group(group) => token_trees_mention_migration(group.stream()),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn is_migration_marker(identifier: &str) -> bool {
    matches!(identifier, "migration" | "MigrationFacade")
}

fn use_tree_contains_migration(tree: &UseTree) -> bool {
    match tree {
        UseTree::Path(path) => path.ident == "migration" || use_tree_contains_migration(&path.tree),
        UseTree::Name(name) => name.ident == "migration",
        UseTree::Rename(rename) => rename.ident == "migration",
        UseTree::Group(group) => group.items.iter().any(use_tree_contains_migration),
        UseTree::Glob(_) => false,
    }
}

fn expression_binding(expression: &Expr, oracle: &Oracle) -> Option<Binding> {
    match expression {
        Expr::Path(path) => oracle.classify_path(&path.path),
        _ => None,
    }
}

fn local_name(pattern: &Pat) -> Option<String> {
    match pattern {
        Pat::Ident(ident) => Some(ident.ident.to_string()),
        _ => None,
    }
}

fn path_segments(path: &SynPath) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect()
}

fn ends_with(path: &[String], suffix: &[&str]) -> bool {
    path.len() >= suffix.len()
        && path[path.len() - suffix.len()..]
            .iter()
            .map(String::as_str)
            .eq(suffix.iter().copied())
}

fn display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}
