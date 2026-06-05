use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{PqtyError, SourceTree, VirtualPath};

pub(super) struct ResolvedSourceFile {
    pub(super) path: VirtualPath,
    pub(super) bytes: Vec<u8>,
}

pub(super) fn enqueue_source_file(
    resolved: Option<ResolvedSourceFile>,
    queue: &mut VecDeque<(VirtualPath, Option<Vec<u8>>)>,
) -> Option<String> {
    resolved.map(|file| {
        let path = file.path.as_str().to_string();
        queue.push_back((file.path, Some(file.bytes)));
        path
    })
}

pub(super) fn read_source_file(
    tree: &impl SourceTree,
    path: &VirtualPath,
) -> Result<Option<Vec<u8>>, PqtyError> {
    tree.read(path).map_err(|source| PqtyError::Io {
        path: PathBuf::from(path.as_str()),
        source,
    })
}

pub(super) fn resolve_tex_input(
    tree: &impl SourceTree,
    current_path: &VirtualPath,
    name: &str,
) -> Result<Option<ResolvedSourceFile>, PqtyError> {
    resolve_with_extensions(tree, current_path, name, &["tex"])
}

pub(super) fn resolve_with_extensions(
    tree: &impl SourceTree,
    current_path: &VirtualPath,
    name: &str,
    extensions: &[&str],
) -> Result<Option<ResolvedSourceFile>, PqtyError> {
    let raw_name = name.trim();
    let name = PathBuf::from(raw_name);
    if name.as_os_str().is_empty() || name.is_absolute() {
        return Ok(None);
    }
    let mut base_names = vec![name];
    if let Some(suffix) = leading_control_sequence_path_suffix(raw_name) {
        base_names.push(PathBuf::from(suffix));
    }
    let mut candidates = Vec::new();
    for name in base_names {
        candidates.push(name.clone());
        if name.extension().is_none() {
            for extension in extensions {
                candidates.push(name.with_extension(extension));
            }
        }
    }

    let current_dir = Path::new(current_path.as_str())
        .parent()
        .filter(|path| !path.as_os_str().is_empty());
    for candidate in candidates {
        let mut paths = Vec::new();
        if let Some(current_dir) = current_dir {
            paths.push(current_dir.join(&candidate));
        }
        paths.push(candidate.clone());
        for path in paths {
            if let Some(path) = normalize_project_path(&path)
                && let Some(bytes) = read_source_file(tree, &path)?
            {
                return Ok(Some(ResolvedSourceFile { path, bytes }));
            }
        }
        if let Some(candidate) = normalize_project_path(&candidate)
            && let Some((path, bytes)) =
                tree.search(&candidate).map_err(|source| PqtyError::Io {
                    path: PathBuf::from(candidate.as_str()),
                    source,
                })?
        {
            return Ok(Some(ResolvedSourceFile { path, bytes }));
        }
    }
    Ok(None)
}

/// Recover the concrete project-relative suffix from a common path idiom such
/// as `\projectroot/styles/local`. The control sequence itself is deliberately
/// not evaluated; resolution succeeds only when the confined suffix names an
/// existing project-owned file.
fn leading_control_sequence_path_suffix(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'\\') {
        return None;
    }
    let name_end = bytes[1..]
        .iter()
        .position(|byte| !(byte.is_ascii_alphabetic() || *byte == b'@'))
        .map_or(bytes.len(), |offset| offset + 1);
    if name_end == 1 || bytes.get(name_end) != Some(&b'/') {
        return None;
    }
    let suffix = &value[name_end + 1..];
    (!suffix.is_empty()
        && !suffix.contains(['\\', '#', '{', '}'])
        && normalize_project_path(Path::new(suffix)).is_some())
    .then_some(suffix)
}

pub(super) fn normalize_project_path(path: &Path) -> Option<VirtualPath> {
    if path.is_absolute() {
        return None;
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| VirtualPath(parts.join("/")))
}

#[cfg(test)]
pub(crate) fn clean_relative_name(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return None;
    }
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => cleaned.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!cleaned.as_os_str().is_empty()).then_some(cleaned)
}

pub(crate) fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}
