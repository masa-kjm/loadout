use std::ffi::OsString;

use clap::{
    Args, ColorChoice, Parser, Subcommand,
    error::{Error, ErrorKind},
};

#[derive(Debug, PartialEq)]
pub(super) enum Command {
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
    Diff,
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
    #[command(about = "Validate declarations without inspecting managed targets.")]
    Validate(ValidateArgs),
    #[command(about = "Show the actions needed to converge a profile.")]
    Plan(ProfileArgs),
    #[command(about = "Converge a profile after safety checks and confirmation.")]
    Apply(ApplyArgs),
    #[command(about = "Report drift between recorded state and managed targets.")]
    Diff,
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

pub(super) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, clap::Error> {
    let cli = Cli::try_parse_from(std::iter::once(OsString::from("loadout")).chain(args))?;
    let command = match cli.command {
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
        ParsedCommand::Diff => Command::Diff,
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
    Ok(command)
}

impl Command {
    fn config(&self) -> Option<&OsString> {
        match self {
            Self::Validate { config, .. }
            | Self::Plan { config, .. }
            | Self::Apply { config, .. } => config.as_ref(),
            Self::Diff => None,
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
    }

    #[test]
    fn rejects_invalid_command_shapes() {
        for args in [
            vec![],
            vec!["unknown"],
            vec!["diff", "--config", "config.yaml"],
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
