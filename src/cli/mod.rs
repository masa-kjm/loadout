//! Terminal adapter. Selection, resolution, observation and planning stay in core layers.

mod apply;
mod args;
mod render;
use crate::application::queries::{
    self, DeclarationRequest, QueryError, StatusDesired, ValidationRequest, ValidationSelection,
};
use crate::authoring::{config_set, config_use, init};
use crate::domain::ids::{FullyQualifiedResourceId, ProfileId};
use crate::loader::{LoadError, MachinePaths, StatePaths};
use crate::state::repository::StateRepository;
use args::Command;
use clap::error::ErrorKind;
use std::{
    io::{self, BufRead, IsTerminal, Write},
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
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                write!(out, "{error}")?;
                return Ok(0);
            }
            writeln!(err, "input error: {error}")?;
            return Ok(error.exit_code() as u8);
        }
    };
    if let Command::Init { dry_run } = command {
        let current_directory = match std::env::current_dir() {
            Ok(path) => path,
            Err(error) => {
                writeln!(err, "error: cannot determine current directory: {error}")?;
                return Ok(1);
            }
        };
        return match init::initialize(&current_directory, dry_run) {
            Ok(report) => {
                if report.dry_run() {
                    writeln!(out, "would create {}", report.root().display())?;
                } else {
                    writeln!(out, "initialized {}", report.root().display())?;
                }
                Ok(0)
            }
            Err(error) => {
                writeln!(err, "error: {error}")?;
                Ok(error.exit_code())
            }
        };
    }
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
        Command::Init { .. } => unreachable!("init returns before machine path selection"),
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
        Command::Status { config, root } => {
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            queries::status(
                &DeclarationRequest { context, root },
                &StateRepository::new(machine.state_directory.clone()),
            )
            .map(|report| {
                let code = status_exit_code(&report);
                render::status(out, &report).map(|()| code)
            })
        }
        Command::Profile(command) => return run_profile(command, machine, out, err),
        Command::Resource(command) => return run_resource(command, machine, out, err),
        Command::Config(command) => return run_config(command, machine, out, err),
    };
    render_result(result, err)
}

fn run_profile(
    command: args::ProfileCommand,
    machine: &MachinePaths,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<u8> {
    match command {
        args::ProfileCommand::List { config } => {
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            render_result(
                queries::profiles(&context)
                    .map(|report| render::profile_list(out, &report).map(|()| 0)),
                err,
            )
        }
        args::ProfileCommand::Show { config, profile_id } => {
            let profile_id = match ProfileId::parse(profile_id) {
                Ok(profile_id) => profile_id,
                Err(error) => return input_error(err, error),
            };
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            match queries::profile(&context, &profile_id) {
                Ok(Some(report)) => render::profile_show(out, &report).map(|()| 0),
                Ok(None) => input_error(err, format!("unknown profile ID: {profile_id}")),
                Err(error) => render_result(Err(error), err),
            }
        }
    }
}

fn run_resource(
    command: args::ResourceCommand,
    machine: &MachinePaths,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<u8> {
    match command {
        args::ResourceCommand::KnownList => render_result(
            queries::known_resources(&StateRepository::new(machine.state_directory.clone()))
                .map(|report| render::known_resources(out, &report).map(|()| 0)),
            err,
        ),
        args::ResourceCommand::KnownShow { resource_id } => {
            let resource_id = match FullyQualifiedResourceId::parse(&resource_id) {
                Ok(resource_id) => resource_id,
                Err(error) => return input_error(err, error),
            };
            match queries::known_resources(&StateRepository::new(machine.state_directory.clone())) {
                Ok(report) => match report
                    .resources
                    .into_iter()
                    .find(|resource| resource.resource_id() == &resource_id)
                {
                    Some(resource) => render::known_resource(out, &resource).map(|()| 0),
                    None => input_error(err, format!("unknown resource ID: {resource_id}")),
                },
                Err(error) => render_result(Err(error), err),
            }
        }
        args::ResourceCommand::DesiredList { config, root } => {
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            render_result(
                queries::desired_resources(&DeclarationRequest { context, root })
                    .map(|report| render::desired_resources(out, &report).map(|()| 0)),
                err,
            )
        }
        args::ResourceCommand::DesiredShow {
            config,
            root,
            resource_id,
        } => {
            let resource_id = match FullyQualifiedResourceId::parse(&resource_id) {
                Ok(resource_id) => resource_id,
                Err(error) => return input_error(err, error),
            };
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            match queries::desired_resources(&DeclarationRequest { context, root }) {
                Ok(report) => match report
                    .resources
                    .iter()
                    .find(|resource| resource.resource_id() == &resource_id)
                {
                    Some(resource) => {
                        render::desired_resource(out, &report.root_profile, resource).map(|()| 0)
                    }
                    None => input_error(err, format!("unknown resource ID: {resource_id}")),
                },
                Err(error) => render_result(Err(error), err),
            }
        }
    }
}

fn status_exit_code(report: &queries::StatusReport) -> u8 {
    match &report.desired {
        StatusDesired::Unavailable(error) => query_exit_code(error),
        StatusDesired::Available { .. } if report.has_unavailable_observation => 1,
        StatusDesired::Available { .. } => 0,
    }
}

fn input_error(err: &mut impl Write, error: impl std::fmt::Display) -> io::Result<u8> {
    writeln!(err, "input error: {error}")?;
    Ok(2)
}

fn run_config(
    command: args::ConfigCommand,
    machine: &MachinePaths,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<u8> {
    let result = match command {
        args::ConfigCommand::Path { config, system } => {
            if system {
                return render::config_path(
                    out,
                    &machine.runtime_directory.as_ref().join("loadout.yaml"),
                );
            }
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            Ok(render::config_path(
                out,
                context.environment_config_path().as_ref(),
            ))
        }
        args::ConfigCommand::List { config } => {
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            queries::configuration_report(&context).map(|report| render::config_list(out, &report))
        }
        args::ConfigCommand::Get { config, field } => {
            let context = match machine.select(config.as_deref()) {
                Ok(context) => context,
                Err(error) => return load_error(err, error),
            };
            queries::configuration_value(&context, &field)
                .map(|value| render::config_get(out, value, &field))
        }
        args::ConfigCommand::Use { path, yes } => {
            return run_config_use(machine, path, yes, out, err);
        }
        args::ConfigCommand::Set {
            config,
            field,
            value,
            yes,
        } => return run_config_set(machine, config, field, value, yes, out, err),
    };
    render_result(result, err)
}

fn run_config_use(
    machine: &MachinePaths,
    path: std::ffi::OsString,
    yes: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<u8> {
    let context = match machine.select(Some(&path)) {
        Ok(context) => context,
        Err(error) => return load_error(err, error),
    };
    if let Err(error) = queries::configuration(&context) {
        return render_result(Err(error), err);
    }
    let destination = machine.runtime_directory.as_ref().join("loadout.yaml");
    let preparation = match config_use::prepare(destination, context.environment_config_path()) {
        Ok(preparation) => preparation,
        Err(error) => return config_use_error(err, error),
    };
    writeln!(
        out,
        "runtime configuration: {}",
        preparation.destination().display()
    )?;
    match preparation.prior() {
        Some(prior) => writeln!(out, "prior config_path: {prior}")?,
        None => writeln!(out, "prior config_path: <absent>")?,
    }
    writeln!(
        out,
        "resulting config_path: {}",
        preparation.selected_path()
    )?;
    out.flush()?;
    if !yes {
        if !(io::stdin().is_terminal() && io::stderr().is_terminal()) {
            writeln!(
                err,
                "confirmation unavailable: non-interactive config use requires --yes"
            )?;
            return Ok(2);
        }
        write!(err, "Use this portable configuration? [y/N] ")?;
        err.flush()?;
        let mut response = String::new();
        io::stdin().lock().read_line(&mut response)?;
        if !matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            writeln!(err, "config use cancelled: confirmation not granted")?;
            return Ok(2);
        }
    }
    match config_use::publish(&preparation) {
        Ok(()) => {
            writeln!(
                out,
                "selected portable configuration: {}",
                preparation.selected_path()
            )?;
            Ok(0)
        }
        Err(error) => config_use_error(err, error),
    }
}

fn config_use_error(err: &mut impl Write, error: config_use::ConfigUseError) -> io::Result<u8> {
    writeln!(err, "error: {error}")?;
    Ok(error.exit_code())
}

fn run_config_set(
    machine: &MachinePaths,
    config: Option<std::ffi::OsString>,
    field: String,
    value: String,
    yes: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<u8> {
    let context = match machine.select(config.as_deref()) {
        Ok(context) => context,
        Err(error) => return load_error(err, error),
    };
    let preparation =
        match config_set::prepare(context.environment_config_path().clone(), &field, &value) {
            Ok(preparation) => preparation,
            Err(error) => return config_set_error(err, error),
        };
    if let Err(error) = queries::configuration_candidate(&context, preparation.candidate()) {
        return render_result(Err(error), err);
    }
    writeln!(
        out,
        "configuration: {}",
        preparation.destination().display()
    )?;
    writeln!(out, "field: {}", preparation.field())?;
    writeln!(out, "prior value: {}", preparation.prior())?;
    writeln!(out, "resulting value: {}", preparation.result())?;
    out.flush()?;
    if !yes {
        if !(io::stdin().is_terminal() && io::stderr().is_terminal()) {
            writeln!(
                err,
                "confirmation unavailable: non-interactive config set requires --yes"
            )?;
            return Ok(2);
        }
        write!(err, "Set this configuration value? [y/N] ")?;
        err.flush()?;
        let mut response = String::new();
        io::stdin().lock().read_line(&mut response)?;
        if !matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            writeln!(err, "config set cancelled: confirmation not granted")?;
            return Ok(2);
        }
    }
    match config_set::publish(&preparation) {
        Ok(()) => {
            writeln!(out, "updated {}", preparation.field())?;
            Ok(0)
        }
        Err(error) => config_set_error(err, error),
    }
}

fn config_set_error(err: &mut impl Write, error: config_set::ConfigSetError) -> io::Result<u8> {
    writeln!(err, "error: {error}")?;
    Ok(error.exit_code())
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
        QueryError::Configuration(_) | QueryError::ConfigField(_) => 2,
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
            fs::write(root.join("config.yaml"), "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: store\n").unwrap();
            fs::write(root.join("profiles/base.yaml"), "schema_version: 2\nid: base\nresources:\n  item:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: ~/absent-parent/target\n").unwrap();
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
    fn inspection_adapters_preserve_their_declared_read_boundaries() {
        let fixture = Fixture::new();
        let config = Some(fixture.root.join("config.yaml").into_os_string());
        fs::write(fixture.root.join("state"), "not a directory").unwrap();
        let _read_only = test_support::forbid_mutation();
        let _no_targets = test_support::forbid_target_inspection();
        for command in [
            Command::Profile(args::ProfileCommand::List {
                config: config.clone(),
            }),
            Command::Profile(args::ProfileCommand::Show {
                config: config.clone(),
                profile_id: "base".into(),
            }),
            Command::Resource(args::ResourceCommand::DesiredList {
                config: config.clone(),
                root: None,
            }),
            Command::Resource(args::ResourceCommand::DesiredShow {
                config,
                root: None,
                resource_id: "base/item".into(),
            }),
        ] {
            assert_eq!(
                run_command(
                    command,
                    &fixture.machine(),
                    &mut Vec::new(),
                    &mut Vec::new()
                )
                .unwrap(),
                0
            );
        }
    }

    #[test]
    fn known_resource_adapters_do_not_read_declarations_or_targets() {
        let fixture = Fixture::new();
        let _read_only = test_support::forbid_mutation();
        let _no_desired = test_support::forbid_desired_dependencies();
        let _no_targets = test_support::forbid_target_inspection();
        assert_eq!(
            run_command(
                Command::Resource(args::ResourceCommand::KnownList),
                &fixture.machine(),
                &mut Vec::new(),
                &mut Vec::new()
            )
            .unwrap(),
            0
        );
        assert_eq!(
            run_command(
                Command::Resource(args::ResourceCommand::KnownShow {
                    resource_id: "base/item".into(),
                }),
                &fixture.machine(),
                &mut Vec::new(),
                &mut Vec::new()
            )
            .unwrap(),
            2
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
                Command::Status {
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

    #[test]
    fn help_and_version_are_successful_stdout_only_requests() {
        let mut output = Vec::new();
        let mut errors = Vec::new();
        assert_eq!(
            run_with(
                ["--help"].into_iter().map(std::ffi::OsString::from),
                &mut output,
                &mut errors,
            )
            .unwrap(),
            0
        );
        let help = String::from_utf8(output).unwrap();
        assert!(help.contains("validate"));
        assert!(help.contains("plan"));
        assert!(help.contains("apply"));
        assert!(help.contains("diff"));
        assert!(errors.is_empty());

        for (arguments, expected_usage) in [
            (vec!["help", "plan"], "Usage: loadout plan"),
            (vec!["validate", "--help"], "Usage: loadout validate"),
        ] {
            let mut output = Vec::new();
            assert_eq!(
                run_with(
                    arguments.into_iter().map(std::ffi::OsString::from),
                    &mut output,
                    &mut errors,
                )
                .unwrap(),
                0
            );
            assert!(String::from_utf8(output).unwrap().contains(expected_usage));
            assert!(errors.is_empty());
        }

        let mut output = Vec::new();
        assert_eq!(
            run_with(
                ["--version"].into_iter().map(std::ffi::OsString::from),
                &mut output,
                &mut errors,
            )
            .unwrap(),
            0
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("loadout {}\n", env!("CARGO_PKG_VERSION"))
        );
        assert!(errors.is_empty());
    }
}
