use std::io;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::PqtyError;
use crate::artifact::validation::validate_portable_path;

/// A normalized, UTF-8 path relative to a project root.
///
/// Values never contain parent traversal, absolute prefixes, backslashes, or
/// empty path segments, so they can safely identify files in any [`SourceTree`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtualPath(pub(crate) String);

impl VirtualPath {
    /// Create a normalized, confined project-relative path.
    ///
    /// # Errors
    ///
    /// Returns an error for absolute, empty, or parent-traversing paths.
    pub fn new(path: impl AsRef<str>) -> Result<Self, PqtyError> {
        let normalized = path.as_ref().replace('\\', "/");
        validate_portable_path(&normalized, "project path")?;
        let path = Path::new(&normalized);
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => {
                    parts.push(
                        part.to_str()
                            .ok_or_else(|| {
                                PqtyError::Usage(format!(
                                    "project path is not valid UTF-8: {}",
                                    path.display()
                                ))
                            })?
                            .to_string(),
                    );
                }
                Component::CurDir
                | Component::ParentDir
                | Component::RootDir
                | Component::Prefix(_) => {
                    return Err(PqtyError::Usage(format!(
                        "project path is not normalized: {}",
                        path.display()
                    )));
                }
            }
        }
        if parts.is_empty() {
            return Err(PqtyError::Usage("project path cannot be empty".to_string()));
        }
        Ok(Self(parts.join("/")))
    }

    #[must_use]
    /// Return the normalized path using `/` separators.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for VirtualPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for VirtualPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let path = String::deserialize(deserializer)?;
        validate_portable_path(&path, "project path").map_err(serde::de::Error::custom)?;
        Ok(Self(path))
    }
}

/// Minimal access to a tree of normalized source paths.
///
/// Renderers and editors adapt their own snapshot or workspace model outside
/// pqty. The package layer only needs to read bytes by virtual path.
pub trait SourceTree {
    /// Read a project-relative path, returning `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns the backing tree's I/O error.
    fn read(&self, path: &VirtualPath) -> io::Result<Option<Vec<u8>>>;

    /// Search explicitly configured, project-owned input roots for a path.
    ///
    /// Snapshot-backed consumers that do not expose search roots keep the
    /// default empty result.
    ///
    /// # Errors
    ///
    /// Returns the backing tree's I/O error.
    fn search(&self, _path: &VirtualPath) -> io::Result<Option<(VirtualPath, Vec<u8>)>> {
        Ok(None)
    }
}
