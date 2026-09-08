//! Machine locations and configuration selection; no managed-target or state access.

use crate::declaration::runtime_config::RuntimeConfig;
use crate::domain::paths::{ResolvedPath, ResolvedPathError};
use crate::resolver::ResolverContext;
use std::{
    env,
    ffi::OsStr,
    fmt, fs, io,
    path::{Component, Path, PathBuf},
};

pub(crate) struct StatePaths {
    pub(crate) home: ResolvedPath,
    pub(crate) state_directory: ResolvedPath,
}

pub(crate) struct MachinePaths {
    pub(crate) home: ResolvedPath,
    pub(crate) runtime_directory: ResolvedPath,
    pub(crate) state_directory: ResolvedPath,
}

#[derive(Debug)]
pub(crate) enum LoadError {
    MissingEnvironment(&'static str),
    Path(ResolvedPathError),
    InvalidSelection(PathBuf),
    Read {
        path: PathBuf,
        source: io::Error,
    },
    RuntimeSchema {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    CurrentDirectory(io::Error),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnvironment(name) => write!(f, "missing machine location: {name}"),
            Self::Path(error) => error.fmt(f),
            Self::InvalidSelection(path) => {
                write!(f, "invalid configuration path: {}", path.display())
            }
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::RuntimeSchema { path, source } => write!(
                f,
                "invalid runtime configuration {}: {source}",
                path.display()
            ),
            Self::CurrentDirectory(error) => {
                write!(f, "cannot determine current directory: {error}")
            }
        }
    }
}

fn location(name: &'static str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

impl StatePaths {
    /// Diff depends only on home and state locations, including on Windows.
    pub(crate) fn from_environment() -> Result<Self, LoadError> {
        #[cfg(unix)]
        let (home_name, state_name) = ("HOME", "XDG_STATE_HOME");
        #[cfg(windows)]
        let (home_name, state_name) = ("USERPROFILE", "LOCALAPPDATA");
        let home = location(home_name).ok_or(LoadError::MissingEnvironment(home_name))?;
        #[cfg(unix)]
        let state = location(state_name).unwrap_or_else(|| home.join(".local/state"));
        #[cfg(windows)]
        let state = location(state_name).ok_or(LoadError::MissingEnvironment(state_name))?;
        Ok(Self {
            home: ResolvedPath::new(home).map_err(LoadError::Path)?,
            state_directory: ResolvedPath::new(state.join("loadout")).map_err(LoadError::Path)?,
        })
    }
}

impl MachinePaths {
    /// Binds environment-provided machine locations without accessing those paths.
    pub(crate) fn from_environment() -> Result<Self, LoadError> {
        let StatePaths {
            home,
            state_directory,
        } = StatePaths::from_environment()?;
        #[cfg(unix)]
        let config = location("XDG_CONFIG_HOME").unwrap_or_else(|| home.as_ref().join(".config"));
        #[cfg(windows)]
        let config = location("APPDATA").ok_or(LoadError::MissingEnvironment("APPDATA"))?;
        Ok(Self {
            home,
            runtime_directory: ResolvedPath::new(config.join("loadout"))
                .map_err(LoadError::Path)?,
            state_directory,
        })
    }

    /// Explicit selection bypasses reading runtime configuration altogether.
    pub(crate) fn select(&self, explicit: Option<&OsStr>) -> Result<ResolverContext, LoadError> {
        let runtime_path = self.runtime_directory.as_ref().join("loadout.yaml");
        let selected = if let Some(path) = explicit {
            if Path::new(path).is_absolute() || path.as_encoded_bytes().starts_with(b"~/") {
                self.bind(path, self.home.as_ref())?
            } else {
                let cwd = env::current_dir().map_err(LoadError::CurrentDirectory)?;
                self.bind(path, &cwd)?
            }
        } else {
            #[cfg(test)]
            crate::test_support::assert_desired_dependencies_allowed();
            match fs::read_to_string(&runtime_path) {
                Ok(yaml) => {
                    let runtime =
                        RuntimeConfig::parse(&yaml).map_err(|source| LoadError::RuntimeSchema {
                            path: runtime_path.clone(),
                            source,
                        })?;
                    self.bind(
                        OsStr::new(runtime.config_path().unwrap_or("config.yaml")),
                        self.runtime_directory.as_ref(),
                    )?
                }
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && fs::symlink_metadata(&runtime_path).is_err_and(|metadata_error| {
                            metadata_error.kind() == io::ErrorKind::NotFound
                        }) =>
                {
                    self.runtime_directory.as_ref().join("config.yaml")
                }
                Err(source) => {
                    return Err(LoadError::Read {
                        path: runtime_path,
                        source,
                    });
                }
            }
        };
        ResolverContext::new(
            self.home.as_ref().to_owned(),
            runtime_path,
            selected,
            self.state_directory.as_ref().to_owned(),
        )
        .map_err(LoadError::Path)
    }

    fn bind(&self, raw: &OsStr, base: &Path) -> Result<PathBuf, LoadError> {
        let path = Path::new(raw);
        if raw.is_empty() {
            return Err(LoadError::InvalidSelection(path.to_owned()));
        }
        let bound = if raw.as_encoded_bytes().starts_with(b"~/") {
            self.home.as_ref().join(
                path.strip_prefix("~")
                    .expect("home-relative prefix was checked"),
            )
        } else if path.is_absolute() {
            path.to_owned()
        } else {
            if raw.as_encoded_bytes().starts_with(b"~")
                || path.has_root()
                || path
                    .to_str()
                    .is_some_and(crate::domain::paths::has_windows_drive_prefix)
            {
                return Err(LoadError::InvalidSelection(path.to_owned()));
            }
            base.join(path)
        };
        // Resolve parent traversal physically; never collapse '..' across a symlink.
        // Keep the final config filename so its containing directory remains the base.
        let normalized = if bound.components().any(|part| part == Component::ParentDir) {
            let parent = bound
                .parent()
                .ok_or_else(|| LoadError::InvalidSelection(bound.clone()))?;
            let filename = bound
                .file_name()
                .ok_or_else(|| LoadError::InvalidSelection(bound.clone()))?;
            fs::canonicalize(parent)
                .map_err(|source| LoadError::Read {
                    path: parent.to_owned(),
                    source,
                })?
                .join(filename)
        } else {
            bound
        };
        ResolvedPath::new(normalized)
            .map(ResolvedPath::into_path_buf)
            .map_err(LoadError::Path)
    }
}
