use std::ffi::OsString;

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

pub(super) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let command = args
        .next()
        .ok_or("expected validate, diff, plan, or apply")?;
    let name = command.to_str().ok_or("command must be UTF-8")?;
    if name == "diff" {
        return if args.next().is_none() {
            Ok(Command::Diff)
        } else {
            Err("diff accepts no arguments".into())
        };
    }
    if name != "validate" && name != "plan" && name != "apply" {
        return Err(format!("unknown command: {name}"));
    }
    let mut config = None;
    let mut root = None;
    let mut all = false;
    let mut yes = false;
    let mut dry_run = false;
    while let Some(arg) = args.next() {
        if arg == "--config" {
            if config.is_some() {
                return Err("--config may only be specified once".into());
            }
            let value = args.next().ok_or("--config requires a path")?;
            if value.is_empty() || value.to_str().is_some_and(|v| v.starts_with("--")) {
                return Err("--config requires a path".into());
            }
            config = Some(value);
        } else if arg == "--all" && name == "validate" {
            if all {
                return Err("--all may only be specified once".into());
            }
            all = true;
        } else if name == "apply" && (arg == "--yes" || arg == "--dry-run") {
            let flag = if arg == "--yes" {
                &mut yes
            } else {
                &mut dry_run
            };
            if *flag {
                return Err(format!(
                    "{} may only be specified once",
                    arg.to_string_lossy()
                ));
            }
            *flag = true;
        } else {
            let value = arg.into_string().map_err(|_| "profile ID must be UTF-8")?;
            if value.starts_with('-') {
                return Err(format!("unknown option: {value}"));
            }
            if root.replace(value).is_some() {
                return Err("only one root profile ID is allowed".into());
            }
        }
    }
    if all && root.is_some() {
        return Err("--all cannot be combined with a root profile ID".into());
    }
    Ok(if name == "validate" {
        Command::Validate { config, root, all }
    } else if name == "apply" {
        Command::Apply {
            config,
            root,
            yes,
            dry_run,
        }
    } else {
        Command::Plan { config, root }
    })
}
