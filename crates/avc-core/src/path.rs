use std::path::{Component, Path, PathBuf};

use crate::{Error, Result};

pub fn validate_repo_path(path: &str) -> Result<()> {
    if path.is_empty() || path.contains('\\') {
        return Err(Error::InvalidPath(path.into()));
    }
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || parsed.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::InvalidPath(path.into()));
    }
    if parsed
        .components()
        .any(|component| matches!(component, Component::CurDir))
    {
        return Err(Error::InvalidPath(path.into()));
    }
    Ok(())
}

pub fn normalize_repo_path(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    let value = path
        .to_str()
        .ok_or_else(|| Error::InvalidPath(path.display().to_string()))?
        .replace('\\', "/");
    // Shells complete directories with a trailing slash, so `avc add data/`
    // and `avc add data` must name one artifact and one pointer file.
    let value = value.trim_end_matches('/').to_owned();
    validate_repo_path(&value)?;
    Ok(value)
}

pub fn pointer_path(path: impl AsRef<Path>) -> Result<PathBuf> {
    Ok(PathBuf::from(format!("{}.avc", normalize_repo_path(path)?)))
}
