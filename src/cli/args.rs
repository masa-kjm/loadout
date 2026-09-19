use std::ffi::OsString;

use clap::{
    Args, ColorChoice, Parser, Subcommand,
    error::{Error, ErrorKind},
};

#[derive(Debug, PartialEq)]
pub(super) enum Command {
    Init {
        dry_run: bool,
    },
    Validate {
        config: Option<OsString>,
        root: Option<String>,
        all: bool,
    },
    Plan {
        config: Option<OsString>,
        root: Option<String>,
    },
    Apply {
        config: Option<OsString>,
        root: Option<String>,
        yes: bool,
        dry_run: bool,
    },
    Config(ConfigCommand),
    Diff,
    Status {
        config: Option<OsString>,
        root: Option<String>,
    },
    Profile(ProfileCommand),
    Resource(ResourceCommand),
}

#[derive(Parser)]
#[command(
    name = "loadout",
    version,
    about = "Converge local environments from explicit desired state.",
    color = ColorChoice::Never
)]
struct Cli {
    #[command(subcommand)]
    command: ParsedCommand,
}

#[derive(Subcommand)]
enum ParsedCommand {
    #[command(about = "Create an initial portable environment bundle in the current directory.")]
    Init(InitArgs),
    #[command(about = "Validate declarations without inspecting managed targets.")]
    Validate(ValidateArgs),
    #[command(about = "Show the actions needed to converge a profile.")]
    Plan(ProfileArgs),
    #[command(about = "Converge a profile after safety checks and confirmation.")]
    Apply(ApplyArgs),
    #[command(about = "Inspect the selected Loadout configuration without writing it.")]
    Config {
        #[command(subcommand)]
        command: ParsedConfigCommand,
    },
    #[command(about = "Report drift between recorded state and managed targets.")]
    Diff,
    #[command(about = "Report Desired, Known, and observed target facts without planning.")]
    Status(ProfileArgs),
    #[command(about = "Inspect discovered portable profile declarations.")]
    Profile {
        #[command(subcommand)]
        command: ParsedProfileCommand,
    },
    #[command(about = "Inspect resolved Desired resources or validated Known records.")]
    Resource {
        #[command(subcommand)]
        command: ParsedResourceCommand,
    },
}

#[derive(Args)]
struct InitArgs {
    #[arg(
        long,
        help = "Show the files that init would create without writing them."
    )]
    dry_run: bool,
}

#[derive(Args)]
struct ProfileArgs {
    #[arg(
        long,
        value_name = "PATH",
        help = "Select the portable environment configuration file."
    )]
    config: Option<OsString>,
    #[arg(value_name = "PROFILE-ID", help = "Select the root profile by its ID.")]
    root: Option<String>,
}

#[derive(Args)]
struct ConfigSelectionArgs {
    #[arg(
        long,
        value_name = "PATH",
        help = "Select the portable environment configuration file."
    )]
    config: Option<OsString>,
}

#[derive(Args)]
struct ConfigPathArgs {
    #[command(flatten)]
    selection: ConfigSelectionArgs,
    #[arg(
        long,
        conflicts_with = "config",
        help = "Print the machine-local runtime configuration path."
    )]
    system: bool,
}

#[derive(Args)]
struct ValidateArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[arg(
        long,
        conflicts_with = "root",
        help = "Validate every discovered profile as a root."
    )]
    all: bool,
}

#[derive(Args)]
struct ApplyArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[arg(long, help = "Proceed without an interactive confirmation prompt.")]
    yes: bool,
    #[arg(
        long,
        help = "Run the apply lifecycle without changing persistent state."
    )]
    dry_run: bool,
}

#[derive(Subcommand)]
enum ParsedProfileCommand {
    List(ConfigSelectionArgs),
    Show {
        #[command(flatten)]
        selection: ConfigSelectionArgs,
        #[arg(value_name = "PROFILE-ID")]
        profile_id: String,
    },
}

#[derive(Subcommand)]
enum ParsedResourceCommand {
    List(ResourceListArgs),
    Show(ResourceShowArgs),
}

#[derive(Args)]
struct ResourceListArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[arg(
        long,
        conflicts_with = "config",
        help = "Inspect only persisted Known records."
    )]
    known: bool,
}

#[derive(Args)]
struct ResourceShowArgs {
    #[arg(long, value_name = "PATH", conflicts_with = "known")]
    config: Option<OsString>,
    #[arg(long, help = "Inspect only a persisted Known record.")]
    known: bool,
    #[arg(value_name = "ID", num_args = 1..=2)]
    ids: Vec<String>,
}

#[derive(Subcommand)]
enum ParsedConfigCommand {
    Path(ConfigPathArgs),
    List(ConfigSelectionArgs),
    Get {
        #[command(flatten)]
        config: ConfigSelectionArgs,
        #[arg(value_name = "FIELD")]
        field: String,
    },
    Use {
        #[arg(value_name = "PATH")]
        path: OsString,
        #[arg(long, help = "Proceed without an interactive confirmation prompt.")]
        yes: bool,
    },
    Set {
        #[command(flatten)]
        config: ConfigSelectionArgs,
        #[arg(value_name = "FIELD")]
        field: String,
        #[arg(value_name = "VALUE")]
        value: String,
        #[arg(long, help = "Proceed without an interactive confirmation prompt.")]
        yes: bool,
    },
}

#[derive(Debug, PartialEq)]
pub(super) enum ConfigCommand {
    Path {
        config: Option<OsString>,
        system: bool,
    },
    List {
        config: Option<OsString>,
    },
    Get {
        config: Option<OsString>,
        field: String,
    },
    Use {
        path: OsString,
        yes: bool,
    },
    Set {
        config: Option<OsString>,
        field: String,
        value: String,
        yes: bool,
    },
}

#[derive(Debug, PartialEq)]
pub(super) enum ProfileCommand {
    List {
        config: Option<OsString>,
    },
    Show {
        config: Option<OsString>,
        profile_id: String,
    },
}

#[derive(Debug, PartialEq)]
pub(super) enum ResourceCommand {
    DesiredList {
        config: Option<OsString>,
        root: Option<String>,
    },
    DesiredShow {
        config: Option<OsString>,
        root: Option<String>,
        resource_id: String,
    },
    KnownList,
    KnownShow {
        resource_id: String,
    },
}

pub(super) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, clap::Error> {
    let cli = Cli::try_parse_from(std::iter::once(OsString::from("loadout")).chain(args))?;
    let command = match cli.command {
        ParsedCommand::Init(args) => Command::Init {
            dry_run: args.dry_run,
        },
        ParsedCommand::Validate(args) => Command::Validate {
            config: args.profile.config,
            root: args.profile.root,
            all: args.all,
        },
        ParsedCommand::Plan(args) => Command::Plan {
            config: args.config,
            root: args.root,
        },
        ParsedCommand::Apply(args) => Command::Apply {
            config: args.profile.config,
            root: args.profile.root,
            yes: args.yes,
            dry_run: args.dry_run,
        },
        ParsedCommand::Config { command } => Command::Config(match command {
            ParsedConfigCommand::Path(args) => ConfigCommand::Path {
                config: args.selection.config,
                system: args.system,
            },
            ParsedConfigCommand::List(args) => ConfigCommand::List {
                config: args.config,
            },
            ParsedConfigCommand::Get { config, field } => ConfigCommand::Get {
                config: config.config,
                field,
            },
            ParsedConfigCommand::Use { path, yes } => ConfigCommand::Use { path, yes },
            ParsedConfigCommand::Set {
                config,
                field,
                value,
                yes,
            } => ConfigCommand::Set {
                config: config.config,
                field,
                value,
                yes,
            },
        }),
        ParsedCommand::Diff => Command::Diff,
        ParsedCommand::Status(args) => Command::Status {
            config: args.config,
            root: args.root,
        },
        ParsedCommand::Profile { command } => Command::Profile(match command {
            ParsedProfileCommand::List(args) => ProfileCommand::List {
                config: args.config,
            },
            ParsedProfileCommand::Show {
                selection,
                profile_id,
            } => ProfileCommand::Show {
                config: selection.config,
                profile_id,
            },
        }),
        ParsedCommand::Resource { command } => Command::Resource(match command {
            ParsedResourceCommand::List(args) if args.known && args.profile.root.is_none() => {
                ResourceCommand::KnownList
            }
            ParsedResourceCommand::List(args) if args.known => {
                return Err(Error::raw(
                    ErrorKind::ArgumentConflict,
                    "resource list --known does not accept a profile ID",
                ));
            }
            ParsedResourceCommand::List(args) => ResourceCommand::DesiredList {
                config: args.profile.config,
                root: args.profile.root,
            },
            ParsedResourceCommand::Show(args) if args.known && args.ids.len() == 1 => {
                ResourceCommand::KnownShow {
                    resource_id: args.ids.into_iter().next().unwrap(),
                }
            }
            ParsedResourceCommand::Show(args) if !args.known && args.ids.len() == 1 => {
                ResourceCommand::DesiredShow {
                    config: args.config,
                    root: None,
                    resource_id: args.ids.into_iter().next().unwrap(),
                }
            }
            ParsedResourceCommand::Show(args) if !args.known && args.ids.len() == 2 => {
                let mut ids = args.ids.into_iter();
                ResourceCommand::DesiredShow {
                    config: args.config,
                    root: Some(ids.next().unwrap()),
                    resource_id: ids.next().unwrap(),
                }
            }
            ParsedResourceCommand::Show(_) => {
                return Err(Error::raw(
                    ErrorKind::ArgumentConflict,
                    "resource show --known requires exactly one resource ID",
                ));
            }
        }),
    };
    if command
        .config()
        .is_some_and(|config| config.as_os_str().is_empty())
    {
        return Err(Error::raw(
            ErrorKind::InvalidValue,
            "--config requires a path",
        ));
    }
    if matches!(&command, Command::Config(ConfigCommand::Use { path, .. }) if path.is_empty()) {
        return Err(Error::raw(
            ErrorKind::InvalidValue,
            "config use requires a path",
        ));
    }
    Ok(command)
}

impl Command {
    fn config(&self) -> Option<&OsString> {
        match self {
            Self::Validate { config, .. }
            | Self::Plan { config, .. }
            | Self::Apply { config, .. }
            | Self::Status { config, .. } => config.as_ref(),
            Self::Config(command) => command.config(),
            Self::Profile(ProfileCommand::List { config })
            | Self::Profile(ProfileCommand::Show { config, .. }) => config.as_ref(),
            Self::Resource(ResourceCommand::DesiredList { config, .. })
            | Self::Resource(ResourceCommand::DesiredShow { config, .. }) => config.as_ref(),
            Self::Init { .. }
            | Self::Diff
            | Self::Resource(ResourceCommand::KnownList)
            | Self::Resource(ResourceCommand::KnownShow { .. }) => None,
        }
    }
}

impl ConfigCommand {
    fn config(&self) -> Option<&OsString> {
        match self {
            Self::Path { config, .. } | Self::List { config } | Self::Get { config, .. } => {
                config.as_ref()
            }
            Self::Use { .. } => None,
            Self::Set { config, .. } => config.as_ref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strings(args: &[&str]) -> Result<Command, clap::Error> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_each_supported_command() {
        assert_eq!(
            parse_strings(&["validate", "--config", "config.yaml", "base"]).unwrap(),
            Command::Validate {
                config: Some(OsString::from("config.yaml")),
                root: Some("base".into()),
                all: false,
            }
        );
        assert_eq!(
            parse_strings(&["config", "use", "config.yaml", "--yes"]).unwrap(),
            Command::Config(ConfigCommand::Use {
                path: OsString::from("config.yaml"),
                yes: true,
            })
        );
        assert_eq!(
            parse_strings(&[
                "config",
                "set",
                "--config",
                "config.yaml",
                "default_profile",
                "work",
                "--yes",
            ])
            .unwrap(),
            Command::Config(ConfigCommand::Set {
                config: Some(OsString::from("config.yaml")),
                field: "default_profile".into(),
                value: "work".into(),
                yes: true,
            })
        );
        assert_eq!(
            parse_strings(&["validate", "--all"]).unwrap(),
            Command::Validate {
                config: None,
                root: None,
                all: true,
            }
        );
        assert_eq!(
            parse_strings(&["plan", "base", "--config", "config.yaml"]).unwrap(),
            Command::Plan {
                config: Some(OsString::from("config.yaml")),
                root: Some("base".into()),
            }
        );
        assert_eq!(
            parse_strings(&["apply", "--yes", "--dry-run"]).unwrap(),
            Command::Apply {
                config: None,
                root: None,
                yes: true,
                dry_run: true,
            }
        );
        assert_eq!(parse_strings(&["diff"]).unwrap(), Command::Diff);
        assert_eq!(
            parse_strings(&["status", "--config", "config.yaml", "base"]).unwrap(),
            Command::Status {
                config: Some(OsString::from("config.yaml")),
                root: Some("base".into()),
            }
        );
        assert_eq!(
            parse_strings(&["profile", "show", "--config", "config.yaml", "base"]).unwrap(),
            Command::Profile(ProfileCommand::Show {
                config: Some(OsString::from("config.yaml")),
                profile_id: "base".into(),
            })
        );
        assert_eq!(
            parse_strings(&["resource", "show", "base", "base/item"]).unwrap(),
            Command::Resource(ResourceCommand::DesiredShow {
                config: None,
                root: Some("base".into()),
                resource_id: "base/item".into(),
            })
        );
        assert_eq!(
            parse_strings(&["resource", "show", "--known", "base/item"]).unwrap(),
            Command::Resource(ResourceCommand::KnownShow {
                resource_id: "base/item".into(),
            })
        );
        assert_eq!(
            parse_strings(&[
                "config",
                "get",
                "--config",
                "config.yaml",
                "default_profile"
            ])
            .unwrap(),
            Command::Config(ConfigCommand::Get {
                config: Some(OsString::from("config.yaml")),
                field: "default_profile".into(),
            })
        );
        assert_eq!(
            parse_strings(&["init", "--dry-run"]).unwrap(),
            Command::Init { dry_run: true }
        );
    }

    #[test]
    fn rejects_invalid_command_shapes() {
        for args in [
            vec![],
            vec!["unknown"],
            vec!["diff", "--config", "config.yaml"],
            vec!["resource", "list", "--known", "base"],
            vec!["resource", "list", "--known", "--config", "config.yaml"],
            vec!["resource", "show", "--known", "base", "base/item"],
            vec!["config", "path", "unexpected"],
            vec!["config", "get"],
            vec!["config", "use", ""],
            vec!["validate", "--all", "base"],
            vec!["validate", "--config"],
            vec!["validate", "--config", ""],
            vec!["plan", "first", "second"],
            vec!["apply", "--yes", "--yes"],
        ] {
            assert!(parse_strings(&args).is_err(), "{args:?} must reject");
        }
    }
}
