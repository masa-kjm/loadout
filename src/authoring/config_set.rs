//! Presentation-preserving edits to an existing portable configuration file.

use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{declaration::environment_config::EnvironmentConfig, domain::paths::ResolvedPath};

static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) struct SetPreparation {
    destination: ResolvedPath,
    original: String,
    candidate: String,
    field: String,
    prior: String,
    result: String,
}

impl SetPreparation {
    pub(crate) fn destination(&self) -> &Path {
        self.destination.as_ref()
    }
    pub(crate) fn field(&self) -> &str {
        &self.field
    }
    pub(crate) fn prior(&self) -> &str {
        &self.prior
    }
    pub(crate) fn result(&self) -> &str {
        &self.result
    }
    pub(crate) fn candidate(&self) -> &str {
        &self.candidate
    }
}

#[derive(Debug)]
pub(crate) enum ConfigSetError {
    Input {
        path: PathBuf,
        message: String,
    },
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Published {
        path: PathBuf,
        message: String,
    },
}

impl ConfigSetError {
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Self::Input { .. } => 2,
            Self::Io { .. } | Self::Published { .. } => 1,
        }
    }
}

impl fmt::Display for ConfigSetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input { path, message } => {
                write!(f, "cannot edit configuration {}: {message}", path.display())
            }
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "cannot {action} {}: {source}", path.display()),
            Self::Published { path, message } => write!(
                f,
                "published configuration is invalid at {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ConfigSetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Reads and edits a supported scalar field without changing unrelated bytes.
pub(crate) fn prepare(
    destination: ResolvedPath,
    field: &str,
    value: &str,
) -> Result<SetPreparation, ConfigSetError> {
    verify_parent(
        destination
            .as_ref()
            .parent()
            .expect("resolved path has a parent"),
    )?;
    verify_regular(destination.as_ref())?;
    let original = fs::read_to_string(destination.as_ref())
        .map_err(|source| io_error("read configuration", destination.as_ref(), source))?;
    let config = EnvironmentConfig::parse(&original)
        .map_err(|source| input(destination.as_ref(), source.to_string()))?;
    let (candidate, prior) = edit(&original, &config, field, value)
        .map_err(|message| input(destination.as_ref(), message))?;
    Ok(SetPreparation {
        destination,
        original,
        candidate,
        field: field.into(),
        prior,
        result: value.into(),
    })
}

/// Atomically replaces the exact prepared portable configuration document.
pub(crate) fn publish(preparation: &SetPreparation) -> Result<(), ConfigSetError> {
    let destination = preparation.destination();
    let parent = destination.parent().expect("resolved path has a parent");
    verify_parent(parent)?;
    verify_original(destination, &preparation.original)?;
    let temporary = create_temporary(parent, &preparation.candidate)?;
    verify_parent(parent)?;
    verify_original(destination, &preparation.original)?;
    replace(&temporary, destination)?;
    verify_published(destination, &preparation.candidate)?;
    sync_parent(parent)?;
    verify_published(destination, &preparation.candidate)
}

fn edit(
    document: &str,
    config: &EnvironmentConfig,
    field: &str,
    value: &str,
) -> Result<(String, String), String> {
    if field == "default_profile" {
        return edit_default_profile(document, value);
    }
    let store = field
        .strip_prefix("stores.")
        .and_then(|tail| tail.strip_suffix(".properties.path"))
        .ok_or_else(|| format!("unsupported configuration field: {field}"))?;
    if config.store(store).is_none() {
        return Err(format!("unsupported configuration field: {field}"));
    }
    let line = store_path_line(document, store)
        .ok_or_else(|| "cannot prove presentation preservation for this store path".to_owned())?;
    replace_scalar(document, line, value)
}

fn edit_default_profile(document: &str, value: &str) -> Result<(String, String), String> {
    if let Some(line) = top_level_line(document, "default_profile") {
        return replace_scalar(document, line, value);
    }
    let schema = top_level_line(document, "schema_version").ok_or_else(|| {
        "cannot prove presentation preservation without schema_version".to_owned()
    })?;
    let insert_at = line_end(document, schema);
    let rendered = yaml_string(value);
    let mut candidate = String::with_capacity(document.len() + rendered.len() + 18);
    candidate.push_str(&document[..insert_at]);
    candidate.push_str(&format!("default_profile: {rendered}\n"));
    candidate.push_str(&document[insert_at..]);
    Ok((candidate, "<absent>".into()))
}

fn store_path_line(document: &str, store: &str) -> Option<usize> {
    let stores = top_level_line(document, "stores")?;
    let stores_indent = indent_at(document, stores)?;
    let mut store_line = None;
    for line in lines_after(document, stores) {
        let indent = indent_at(document, line)?;
        if indent <= stores_indent && !is_blank_or_comment(line_text(document, line)?) {
            break;
        }
        if indent == stores_indent + 2 && key_at(document, line, store) {
            store_line = Some(line);
            break;
        }
    }
    let store_line = store_line?;
    let mut properties = None;
    for line in lines_after(document, store_line) {
        let indent = indent_at(document, line)?;
        if indent <= stores_indent + 2 && !is_blank_or_comment(line_text(document, line)?) {
            break;
        }
        if indent == stores_indent + 4 && key_at(document, line, "properties") {
            properties = Some(line);
            break;
        }
    }
    let properties = properties?;
    for line in lines_after(document, properties) {
        let indent = indent_at(document, line)?;
        if indent <= stores_indent + 4 && !is_blank_or_comment(line_text(document, line)?) {
            break;
        }
        if indent == stores_indent + 6 && key_at(document, line, "path") {
            return Some(line);
        }
    }
    None
}

fn replace_scalar(document: &str, line: usize, value: &str) -> Result<(String, String), String> {
    let text = line_text(document, line).ok_or_else(|| "invalid YAML line".to_owned())?;
    let colon = text
        .find(':')
        .ok_or_else(|| "target is not a scalar mapping entry".to_owned())?;
    let value_start = line + colon + 1;
    let raw = &document[value_start..line_end(document, line)];
    let (start, end) = scalar_bounds(raw)?;
    let prior = raw[start..end].to_owned();
    let replacement = yaml_string(value);
    let absolute_start = value_start + start;
    let absolute_end = value_start + end;
    let mut candidate = String::with_capacity(document.len() + replacement.len());
    candidate.push_str(&document[..absolute_start]);
    candidate.push_str(&replacement);
    candidate.push_str(&document[absolute_end..]);
    Ok((candidate, prior))
}

fn scalar_bounds(raw: &str) -> Result<(usize, usize), String> {
    let leading = raw.len() - raw.trim_start_matches([' ', '\t']).len();
    let without_newline = raw.trim_end_matches(['\r', '\n']);
    let comment = comment_start(without_newline).unwrap_or(without_newline.len());
    let value = &without_newline[leading..comment];
    let end = leading + value.trim_end_matches([' ', '\t']).len();
    if end == leading
        || value.trim_start().starts_with(['|', '>', '&', '*'])
        || value.contains('\n')
    {
        return Err("cannot prove presentation preservation for this scalar value".into());
    }
    Ok((leading, end))
}

fn comment_start(value: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if quote == Some('"') && escaped {
            escaped = false;
            continue;
        }
        if quote == Some('"') && character == '\\' {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if character == '#' && quote.is_none() && value[..index].ends_with(char::is_whitespace) {
            return Some(index);
        }
    }
    None
}

fn yaml_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}

fn top_level_line(document: &str, key: &str) -> Option<usize> {
    lines(document)
        .find(|line| indent_at(document, *line) == Some(0) && key_at(document, *line, key))
}

fn lines(document: &str) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0)
        .chain(document.match_indices('\n').map(|(i, _)| i + 1))
        .filter(|i| *i < document.len())
}
fn lines_after(document: &str, line: usize) -> impl Iterator<Item = usize> + '_ {
    lines(document).filter(move |candidate| *candidate > line)
}
fn line_end(document: &str, line: usize) -> usize {
    document[line..]
        .find('\n')
        .map(|offset| line + offset)
        .unwrap_or(document.len())
}
fn line_text(document: &str, line: usize) -> Option<&str> {
    document.get(line..line_end(document, line))
}
fn indent_at(document: &str, line: usize) -> Option<usize> {
    let text = line_text(document, line)?;
    Some(text.len() - text.trim_start_matches(' ').len())
}
fn key_at(document: &str, line: usize, key: &str) -> bool {
    line_text(document, line).is_some_and(|text| {
        text.trim_start_matches(' ')
            .strip_prefix(key)
            .is_some_and(|rest| rest.starts_with(':'))
    })
}
fn is_blank_or_comment(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

fn verify_parent(parent: &Path) -> Result<(), ConfigSetError> {
    for component in prefixes(parent)? {
        let metadata = fs::symlink_metadata(&component)
            .map_err(|source| io_error("inspect configuration parent", &component, source))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(input(&component, "expected a non-symlink directory"));
        }
        let declared = ResolvedPath::new(component.clone())
            .map_err(|source| input(&component, source.to_string()))?;
        let observed = ResolvedPath::from_platform_canonicalized(
            fs::canonicalize(&component).map_err(|source| {
                io_error(
                    "verify configuration parent association",
                    &component,
                    source,
                )
            })?,
        )
        .map_err(|source| input(&component, source.to_string()))?;
        if declared != observed {
            return Err(input(
                &component,
                "parent is not associated with its declared path",
            ));
        }
    }
    Ok(())
}

fn prefixes(path: &Path) -> Result<Vec<PathBuf>, ConfigSetError> {
    if !path.is_absolute() {
        return Err(input(path, "configuration parent is not absolute"));
    }
    let mut current = PathBuf::new();
    let mut result = Vec::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if current.is_absolute() {
            result.push(current.clone());
        }
    }
    Ok(result)
}

fn verify_regular(path: &Path) -> Result<(), ConfigSetError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(input(path, "expected a regular non-symlink file")),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            Err(input(path, "configuration file is absent"))
        }
        Err(source) => Err(io_error("inspect configuration", path, source)),
    }
}

fn verify_original(path: &Path, original: &str) -> Result<(), ConfigSetError> {
    verify_regular(path)?;
    let observed = fs::read_to_string(path)
        .map_err(|source| io_error("re-read configuration before publication", path, source))?;
    if observed == original {
        Ok(())
    } else {
        Err(input(path, "configuration changed before publication"))
    }
}

fn create_temporary(parent: &Path, candidate: &str) -> Result<PathBuf, ConfigSetError> {
    for _ in 0..128 {
        let path = parent.join(format!(
            ".loadout-set-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(candidate.as_bytes())
                    .map_err(|source| io_error("write configuration temporary", &path, source))?;
                file.sync_all()
                    .map_err(|source| io_error("flush configuration temporary", &path, source))?;
                drop(file);
                EnvironmentConfig::parse(&fs::read_to_string(&path).map_err(|source| {
                    io_error("re-open configuration temporary", &path, source)
                })?)
                .map_err(|source| ConfigSetError::Published {
                    path: path.clone(),
                    message: source.to_string(),
                })?;
                return Ok(path);
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(io_error("create configuration temporary", &path, source)),
        }
    }
    Err(io_error(
        "allocate configuration temporary",
        parent,
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary name collision limit reached",
        ),
    ))
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn replace(temporary: &Path, destination: &Path) -> Result<(), ConfigSetError> {
    fs::rename(temporary, destination)
        .map_err(|source| io_error("replace configuration", destination, source))
}
#[cfg(windows)]
fn replace(temporary: &Path, destination: &Path) -> Result<(), ConfigSetError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};
    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: the paths are nul-terminated UTF-16 strings that remain valid for the call.
    if unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING,
        )
    } == 0
    {
        return Err(io_error(
            "replace configuration",
            destination,
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}
#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
fn replace(_: &Path, destination: &Path) -> Result<(), ConfigSetError> {
    Err(input(
        destination,
        "atomic replacement is unsupported on this platform",
    ))
}

fn verify_published(path: &Path, candidate: &str) -> Result<(), ConfigSetError> {
    verify_parent(path.parent().expect("resolved path has a parent"))?;
    verify_regular(path)?;
    let observed = fs::read_to_string(path)
        .map_err(|source| io_error("re-open published configuration", path, source))?;
    if observed != candidate {
        return Err(ConfigSetError::Published {
            path: path.to_owned(),
            message: "contents differ from the candidate document".into(),
        });
    }
    EnvironmentConfig::parse(&observed).map_err(|source| ConfigSetError::Published {
        path: path.to_owned(),
        message: source.to_string(),
    })?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), ConfigSetError> {
    use std::fs::File;
    File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error("flush configuration parent", parent, source))
}
#[cfg(not(unix))]
fn sync_parent(_: &Path) -> Result<(), ConfigSetError> {
    Ok(())
}

fn input(path: &Path, message: impl Into<String>) -> ConfigSetError {
    ConfigSetError::Input {
        path: path.to_owned(),
        message: message.into(),
    }
}
fn io_error(action: &'static str, path: &Path, source: io::Error) -> ConfigSetError {
    ConfigSetError::Io {
        action,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCUMENT: &str = "# heading\nschema_version: 2 # schema note\ndefault_profile: base # selected profile\nprofile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: ../store # source root\n";

    #[test]
    fn edits_only_the_supported_default_profile_scalar() {
        let config = EnvironmentConfig::parse(DOCUMENT).unwrap();
        let (candidate, prior) = edit(DOCUMENT, &config, "default_profile", "work").unwrap();

        assert_eq!(prior, "base");
        assert_eq!(
            candidate,
            "# heading\nschema_version: 2 # schema note\ndefault_profile: \"work\" # selected profile\nprofile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: ../store # source root\n"
        );
    }

    #[test]
    fn edits_only_the_selected_store_path_scalar() {
        let config = EnvironmentConfig::parse(DOCUMENT).unwrap();
        let (candidate, prior) = edit(
            DOCUMENT,
            &config,
            "stores.files.properties.path",
            "../other store",
        )
        .unwrap();

        assert_eq!(prior, "../store");
        assert_eq!(
            candidate,
            "# heading\nschema_version: 2 # schema note\ndefault_profile: base # selected profile\nprofile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: \"../other store\" # source root\n"
        );
    }

    #[test]
    fn rejects_presentation_it_cannot_edit_without_collateral_changes() {
        let document = DOCUMENT.replace("path: ../store", "path: |\n        ../store");
        let config = EnvironmentConfig::parse(&document).unwrap();

        assert!(edit(&document, &config, "stores.files.properties.path", "other").is_err());
    }
}
