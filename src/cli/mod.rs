//! Terminal adapter. Selection, resolution, observation and planning stay in core layers.

mod apply;
mod args;
mod render;
use crate::application::queries::{
    self, DeclarationRequest, QueryError, ValidationRequest, ValidationSelection,
};
use crate::loader::{LoadError, MachinePaths, StatePaths};
use crate::state::repository::StateRepository;
use args::Command;
use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

pub(crate) fn run() -> ExitCode {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    match run_with(std::env::args_os().skip(1), &mut stdout, &mut stderr) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = writeln!(stderr, "CLI I/O error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_with(
    args: impl IntoIterator<Item = std::ffi::OsString>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<u8> {
    let command = match args::parse(args) {
        Ok(command) => command,
        Err(message) => {
            writeln!(err, "input error: {message}")?;
            return Ok(2);
        }
    };
    if command == Command::Diff {
        let paths = match StatePaths::from_environment() {
            Ok(paths) => paths,
            Err(error) => return load_error(err, error),
        };
        return render_result(
            queries::diff(&paths.home, &StateRepository::new(paths.state_directory))
                .map(|report| render::diff(out, &report)),
            err,
        );
    }
    let machine = match MachinePaths::from_environment() {
        Ok(machine) => machine,
        Err(error) => return load_error(err, error),
    };
    run_command(command, &machine, out, err)
}

fn run_command(
    command: Command,
    machine: &MachinePaths,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<u8> {
    let result = match command {
        Command::Diff => queries::diff(
            &machine.home,
            &StateRepository::new(machine.state_directory.clone()),
        )
        .map(|report| render::diff(out, &report)),
        Command::Validate { config, root, all } => {
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            let selection = if all {
                ValidationSelection::All
            } else {
                ValidationSelection::Root(root)
            };
            queries::validate(&ValidationRequest { context, selection })
                .map(|report| render::validation(out, err, &report))
        }
        Command::Apply {
            config,
            root,
            yes,
            dry_run,
        } => {
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            return apply::run(
                &DeclarationRequest { context, root },
                yes,
                dry_run,
                &mut io::stdin().lock(),
                io::stdin().is_terminal() && io::stderr().is_terminal(),
                out,
                err,
            );
        }
        Command::Plan { config, root } => {
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            queries::plan_request(&DeclarationRequest { context, root })
                .map(|report| render::plan(out, err, &report))
        }
    };
    render_result(result, err)
}

fn render_result(
    result: Result<io::Result<u8>, QueryError>,
    err: &mut impl Write,
) -> io::Result<u8> {
    match result {
        Ok(rendered) => rendered,
        Err(error) => {
            let code = query_exit_code(&error);
            writeln!(err, "error: {}", render::query_error(&error))?;
            Ok(code)
        }
    }
}

fn load_error(err: &mut impl Write, error: LoadError) -> io::Result<u8> {
    let code = match &error {
        LoadError::Read { .. } | LoadError::CurrentDirectory(_) => 1,
        _ => 2,
    };
    writeln!(err, "error: {error}")?;
    Ok(code)
}

fn query_exit_code(error: &QueryError) -> u8 {
    match error {
        QueryError::ConfigurationRead { .. } | QueryError::State(_) | QueryError::Inspection(_) => {
            1
        }
        QueryError::Resolution(error) => resolver_exit_code(error),
        QueryError::Configuration(_) => 2,
    }
}

fn resolver_exit_code(error: &crate::resolver::ResolverError) -> u8 {
    use crate::resolver::ResolverError;
    match error {
        ResolverError::ReadProfile { .. } | ResolverError::HomeDirectoryIo { .. } => 1,
        ResolverError::ProfileDiscoveryIo { source, .. }
            if source.kind() != io::ErrorKind::NotFound =>
        {
            1
        }
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{domain::paths::ResolvedPath, test_support};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loadout-cli-boundary-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            for dir in ["home", "profiles", "store"] {
                fs::create_dir(root.join(dir)).unwrap();
            }
            fs::write(root.join("store/source"), "source").unwrap();
            fs::write(root.join("config.yaml"), "schema_version: 1\ndefault_profile: base\nprofile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    path: store\n").unwrap();
            fs::write(root.join("profiles/base.yaml"), "schema_version: 1\nid: base\nresources:\n  item:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: ~/absent-parent/target\n").unwrap();
            Self { root }
        }
        fn machine(&self) -> MachinePaths {
            MachinePaths {
                home: ResolvedPath::new(self.root.join("home")).unwrap(),
                runtime_directory: ResolvedPath::new(self.root.join("runtime")).unwrap(),
                state_directory: ResolvedPath::new(self.root.join("state")).unwrap(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn dry_run_adapter_cannot_enter_any_mutating_boundary() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.root.join("home/absent-parent")).unwrap();
        let _read_only = test_support::forbid_mutation();
        for yes in [false, true] {
            assert_eq!(
                run_command(
                    Command::Apply {
                        config: Some(fixture.root.join("config.yaml").into_os_string()),
                        root: None,
                        yes,
                        dry_run: true,
                    },
                    &fixture.machine(),
                    &mut Vec::new(),
                    &mut Vec::new()
                )
                .unwrap(),
                0
            );
        }
        assert!(!fixture.root.join("state").exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_plan_output_cannot_authorize_execution_even_with_yes() {
        struct Unwritable;
        impl Write for Unwritable {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected output failure"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let fixture = Fixture::new();
        fs::create_dir(fixture.root.join("home/absent-parent")).unwrap();
        let request = DeclarationRequest {
            context: fixture
                .machine()
                .select(Some(fixture.root.join("config.yaml").as_os_str()))
                .unwrap(),
            root: None,
        };
        let error = apply::run(
            &request,
            true,
            false,
            &mut io::empty(),
            false,
            &mut Unwritable,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected output failure"));
        assert!(!fixture.root.join("state/state.json").exists());
        assert!(!fixture.root.join("home/absent-parent/target").exists());
    }

    #[test]
    fn validate_adapter_cannot_observe_targets_or_access_state() {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("state"), "not a directory").unwrap();
        let _read_only = test_support::forbid_mutation();
        let _no_targets = test_support::forbid_target_inspection();
        let code = run_command(
            Command::Validate {
                config: Some(fixture.root.join("config.yaml").into_os_string()),
                root: None,
                all: false,
            },
            &fixture.machine(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn diff_adapter_cannot_use_desired_dependencies_or_effect_boundaries() {
        let fixture = Fixture::new();
        let _read_only = test_support::forbid_mutation();
        let _no_desired = test_support::forbid_desired_dependencies();
        assert_eq!(
            run_command(
                Command::Diff,
                &fixture.machine(),
                &mut Vec::new(),
                &mut Vec::new()
            )
            .unwrap(),
            0
        );
        assert!(!fixture.root.join("state").exists());
    }

    #[test]
    fn state_dependent_adapters_reject_invalid_state_before_target_observation() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.root.join("state")).unwrap();
        let _read_only = test_support::forbid_mutation();
        let _no_targets = test_support::forbid_target_inspection();
        for state in [
            "corrupt",
            "{\"schema_version\":9,\"resources\":{},\"active_operation\":null}",
        ] {
            fs::write(fixture.root.join("state/state.json"), state).unwrap();
            for command in [
                Command::Diff,
                Command::Plan {
                    config: Some(fixture.root.join("config.yaml").into_os_string()),
                    root: None,
                },
            ] {
                assert_eq!(
                    run_command(
                        command,
                        &fixture.machine(),
                        &mut Vec::new(),
                        &mut Vec::new()
                    )
                    .unwrap(),
                    1
                );
            }
        }
    }
}
